use std::collections::HashMap;
use std::io::Read;

use flate2::read::ZlibDecoder;
use prost::Message;

use crate::connection::CmConnection;
use crate::crypto::aes_ecb_decrypt;
use crate::error::{Error, Result};

const CM_RETRIES: u32 = 3;
const CM_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
use crate::proto::{
    CContentServerDirectoryGetCdnAuthTokenRequest as CdnAuthTokenRequest,
    CContentServerDirectoryGetCdnAuthTokenResponse as CdnAuthTokenResponse,
    CContentServerDirectoryGetManifestRequestCodeRequest as ManifestCodeRequest,
    CContentServerDirectoryGetManifestRequestCodeResponse as ManifestCodeResponse,
    CContentServerDirectoryGetServersForSteamPipeRequest as SteamPipeRequest,
    CContentServerDirectoryGetServersForSteamPipeResponse as SteamPipeResponse,
    CContentServerDirectoryServerInfo, ContentManifestMetadata, ContentManifestPayload,
    ContentManifestSignature,
};

const PAYLOAD_MAGIC: u32 = 0x71F617D0;
const METADATA_MAGIC: u32 = 0x1F4812BE;
const SIGNATURE_MAGIC: u32 = 0x1B81B817;

/// A CDN server endpoint.
#[derive(Debug, Clone)]
pub struct CdnServer {
    pub host: String,
    pub vhost: String,
    pub server_type: String,
    pub load: i32,
    pub cell_id: i32,
    /// URL scheme used for CDN requests. Normally `https`; local mirrors and
    /// the test suite use `http`.
    pub scheme: String,
}

impl CdnServer {
    /// Build the full URL for a CDN-relative path, e.g.
    /// `/depot/123/manifest/456/5/789`.
    pub fn url(&self, path: &str) -> String {
        format!("{}://{}{}", self.scheme, self.vhost, path)
    }
}

/// A cached CDN auth token.
#[derive(Debug, Clone)]
pub struct CdnAuthToken {
    pub token: String,
    pub expiration: u32,
}

/// A pool of CDN servers with penalty-based selection.
///
/// Servers are sorted by penalty score (lowest first). On transient failures,
/// callers should [`penalize`](CdnPool::penalize) the failing host so future
/// picks rotate to healthier servers.
pub struct CdnPool {
    servers: Vec<CdnServer>,
    penalties: HashMap<String, u32>,
    /// CDN auth tokens keyed by `"depot_id:host"`.
    cdn_auth_tokens: HashMap<String, CdnAuthToken>,
}

impl CdnPool {
    /// Build from an existing server list (already geo/load sorted by Steam).
    pub fn new(servers: Vec<CdnServer>) -> Self {
        Self {
            servers,
            penalties: HashMap::new(),
            cdn_auth_tokens: HashMap::new(),
        }
    }

    /// Pick the best server: lowest penalty score, breaking ties by list order.
    ///
    /// Panics if the server list is empty.
    pub fn pick_server(&self) -> &CdnServer {
        self.servers
            .iter()
            .min_by_key(|s| self.penalties.get(&s.host).copied().unwrap_or(0))
            .expect("CdnPool has no servers")
    }

    /// Increment penalty for a host (called on transient HTTP failures).
    pub fn penalize(&mut self, host: &str) {
        *self.penalties.entry(host.to_string()).or_insert(0) += 1;
    }

    /// Get a cached CDN auth token for this depot+host, if still valid.
    pub fn get_cdn_auth_token(&self, depot_id: u32, host: &str) -> Option<&CdnAuthToken> {
        let key = format!("{}:{}", depot_id, host);
        self.cdn_auth_tokens.get(&key).filter(|t| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0);
            t.expiration > now
        })
    }

    /// Store a CDN auth token.
    pub fn set_cdn_auth_token(&mut self, depot_id: u32, host: &str, token: CdnAuthToken) {
        let key = format!("{}:{}", depot_id, host);
        self.cdn_auth_tokens.insert(key, token);
    }
}

/// Parsed depot manifest from CDN.
#[derive(Debug)]
pub struct DepotManifest {
    pub payload: ContentManifestPayload,
    pub metadata: ContentManifestMetadata,
    pub signature: ContentManifestSignature,
}

/// Get a manifest request code for a specific depot/manifest/branch combination.
///
/// The returned code is time-limited (~5 min) and must be passed in the CDN URL.
/// A code of `0` means the manifest cannot be downloaded.
pub async fn get_manifest_request_code(
    conn: &mut CmConnection,
    app_id: u32,
    depot_id: u32,
    manifest_id: u64,
    branch: &str,
) -> Result<u64> {
    let body = ManifestCodeRequest {
        app_id: Some(app_id),
        depot_id: Some(depot_id),
        manifest_id: Some(manifest_id),
        app_branch: Some(branch.to_string()),
        branch_password_hash: None,
    };
    let encoded = body.encode_to_vec();

    let mut last_err = None;
    for attempt in 0..=CM_RETRIES {
        if attempt > 0 {
            eprintln!(
                "retrying GetManifestRequestCode for depot {} (attempt {}/{})",
                depot_id,
                attempt + 1,
                CM_RETRIES + 1
            );
            tokio::time::sleep(CM_RETRY_DELAY * attempt).await;
        }

        match conn
            .service_method_call("ContentServerDirectory.GetManifestRequestCode#1", &encoded)
            .await
        {
            Ok(resp_bytes) => {
                let resp = ManifestCodeResponse::decode(resp_bytes.as_slice())?;
                let code = resp.manifest_request_code.unwrap_or(0);
                if code == 0 {
                    return Err(Error::Other(format!(
                        "manifest request code denied for depot {} manifest {}",
                        depot_id, manifest_id
                    )));
                }
                return Ok(code);
            }
            Err(e) if is_cm_retryable(&e) && attempt < CM_RETRIES => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap())
}

/// Fetch CDN server list for content downloads.
pub async fn get_cdn_servers(conn: &mut CmConnection, cell_id: u32) -> Result<Vec<CdnServer>> {
    let body = SteamPipeRequest {
        cell_id: Some(cell_id),
        max_servers: Some(20),
        ..Default::default()
    };
    let encoded = body.encode_to_vec();

    let mut last_err = None;
    for attempt in 0..=CM_RETRIES {
        if attempt > 0 {
            eprintln!(
                "retrying GetServersForSteamPipe (attempt {}/{})",
                attempt + 1,
                CM_RETRIES + 1
            );
            tokio::time::sleep(CM_RETRY_DELAY * attempt).await;
        }

        match conn
            .service_method_call("ContentServerDirectory.GetServersForSteamPipe#1", &encoded)
            .await
        {
            Ok(resp_bytes) => {
                let resp = SteamPipeResponse::decode(resp_bytes.as_slice())?;
                return Ok(resp
                    .servers
                    .into_iter()
                    .map(|s: CContentServerDirectoryServerInfo| CdnServer {
                        host: s.host.unwrap_or_default(),
                        vhost: s.vhost.unwrap_or_default(),
                        server_type: s.r#type.unwrap_or_default(),
                        load: s.load.unwrap_or(0),
                        cell_id: s.cell_id.unwrap_or(0),
                        scheme: "https".to_string(),
                    })
                    .collect());
            }
            Err(e) if is_cm_retryable(&e) && attempt < CM_RETRIES => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap())
}

/// Request a CDN auth token for a depot+host via the CM.
///
/// Called lazily when a CDN returns HTTP 403 (authorization required).
pub async fn request_cdn_auth_token(
    conn: &mut CmConnection,
    depot_id: u32,
    host: &str,
    app_id: u32,
) -> Result<CdnAuthToken> {
    let body = CdnAuthTokenRequest {
        depot_id: Some(depot_id),
        host_name: Some(host.to_string()),
        app_id: Some(app_id),
    };
    let encoded = body.encode_to_vec();

    let mut last_err = None;
    for attempt in 0..=CM_RETRIES {
        if attempt > 0 {
            eprintln!(
                "retrying GetCDNAuthToken for depot {} (attempt {}/{})",
                depot_id,
                attempt + 1,
                CM_RETRIES + 1
            );
            tokio::time::sleep(CM_RETRY_DELAY * attempt).await;
        }

        match conn
            .service_method_call("ContentServerDirectory.GetCDNAuthToken#1", &encoded)
            .await
        {
            Ok(resp_bytes) => {
                let resp = CdnAuthTokenResponse::decode(resp_bytes.as_slice())?;
                return Ok(CdnAuthToken {
                    token: resp.token.unwrap_or_default(),
                    expiration: resp.expiration_time.unwrap_or(0),
                });
            }
            Err(e) if is_cm_retryable(&e) && attempt < CM_RETRIES => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap())
}

/// Check if a CM service method error is worth retrying.
fn is_cm_retryable(e: &Error) -> bool {
    matches!(
        e,
        Error::ServiceMethodTimeout(_) | Error::ConnectionClosed | Error::WebSocket(_)
    )
}

/// Download and parse a depot manifest from CDN.
///
/// 1. HTTP GET `https://<host>/depot/<depot_id>/manifest/<manifest_id>/5/<code>`
/// 2. AES-256-ECB decrypt with depot key
/// 3. Zlib decompress
/// 4. Parse protobuf sections (payload, metadata, signature)
pub async fn download_manifest(
    client: &reqwest::Client,
    cdn: &CdnServer,
    depot_id: u32,
    manifest_id: u64,
    request_code: u64,
    depot_key: &[u8],
    cdn_auth_token: Option<&str>,
) -> Result<DepotManifest> {
    let mut url = cdn.url(&format!(
        "/depot/{}/manifest/{}/5/{}",
        depot_id, manifest_id, request_code
    ));
    if let Some(token) = cdn_auth_token {
        url.push_str(token);
    }

    let resp = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| Error::Other(format!("CDN manifest download failed: {}", e)))?;
    let data = resp.bytes().await?;

    // The CDN returns a zip archive containing the manifest.
    // First two bytes 'PK' (0x50, 0x4b) = zip format.
    // Otherwise try zlib, then raw protobuf.
    let manifest_bytes = if data.len() >= 2 && data[0] == 0x50 && data[1] == 0x4b {
        extract_zip(&data)?
    } else if data.len() % 16 == 0 {
        let decrypted = aes_ecb_decrypt(depot_key, &data)?;
        zlib_decompress(&decrypted)?
    } else {
        zlib_decompress(&data)?
    };

    parse_manifest(&manifest_bytes)
}

fn extract_zip(data: &[u8]) -> Result<Vec<u8>> {
    let cursor = std::io::Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| Error::Other(format!("zip open: {}", e)))?;

    if archive.is_empty() {
        return Err(Error::Other("zip archive is empty".into()));
    }

    let mut file = archive
        .by_index(0)
        .map_err(|e| Error::Other(format!("zip entry: {}", e)))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}

fn parse_manifest(data: &[u8]) -> Result<DepotManifest> {
    let mut offset = 0;

    let (magic, payload_data) = read_section(data, &mut offset)?;
    if magic != PAYLOAD_MAGIC {
        return Err(Error::Other(format!(
            "bad payload magic: expected {:08x}, got {:08x}",
            PAYLOAD_MAGIC, magic
        )));
    }
    let payload = ContentManifestPayload::decode(payload_data)?;

    let (magic, metadata_data) = read_section(data, &mut offset)?;
    if magic != METADATA_MAGIC {
        return Err(Error::Other(format!(
            "bad metadata magic: expected {:08x}, got {:08x}",
            METADATA_MAGIC, magic
        )));
    }
    let metadata = ContentManifestMetadata::decode(metadata_data)?;

    let (magic, sig_data) = read_section(data, &mut offset)?;
    if magic != SIGNATURE_MAGIC {
        return Err(Error::Other(format!(
            "bad signature magic: expected {:08x}, got {:08x}",
            SIGNATURE_MAGIC, magic
        )));
    }
    let signature = ContentManifestSignature::decode(sig_data)?;

    Ok(DepotManifest {
        payload,
        metadata,
        signature,
    })
}

fn read_section<'a>(data: &'a [u8], offset: &mut usize) -> Result<(u32, &'a [u8])> {
    if *offset + 8 > data.len() {
        return Err(Error::Other("manifest section header truncated".into()));
    }

    let magic = u32::from_le_bytes(data[*offset..*offset + 4].try_into().unwrap());
    let len = u32::from_le_bytes(data[*offset + 4..*offset + 8].try_into().unwrap()) as usize;
    *offset += 8;

    if *offset + len > data.len() {
        return Err(Error::Other("manifest section body truncated".into()));
    }

    let section = &data[*offset..*offset + len];
    *offset += len;

    Ok((magic, section))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;
    use crate::proto::content_manifest_payload;

    fn server(host: &str) -> CdnServer {
        CdnServer {
            host: host.to_string(),
            vhost: format!("{host}.example.com"),
            server_type: "CDN".to_string(),
            load: 0,
            cell_id: 0,
            scheme: "https".to_string(),
        }
    }

    fn section(magic: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&magic.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn sample_manifest() -> Vec<u8> {
        let payload = ContentManifestPayload {
            mappings: vec![content_manifest_payload::FileMapping {
                filename: Some("game/bin/linux64/server".into()),
                size: Some(3),
                flags: Some(0x20),
                ..Default::default()
            }],
        };
        let metadata = ContentManifestMetadata {
            depot_id: Some(7),
            gid_manifest: Some(42),
            filenames_encrypted: Some(false),
            ..Default::default()
        };
        let signature = ContentManifestSignature::default();

        [
            section(PAYLOAD_MAGIC, &payload.encode_to_vec()),
            section(METADATA_MAGIC, &metadata.encode_to_vec()),
            section(SIGNATURE_MAGIC, &signature.encode_to_vec()),
        ]
        .concat()
    }

    #[test]
    fn parses_manifest_sections() {
        let manifest = parse_manifest(&sample_manifest()).unwrap();

        assert_eq!(manifest.metadata.depot_id, Some(7));
        assert_eq!(manifest.metadata.gid_manifest, Some(42));
        assert_eq!(manifest.payload.mappings.len(), 1);
        assert_eq!(
            manifest.payload.mappings[0].filename.as_deref(),
            Some("game/bin/linux64/server")
        );
    }

    #[test]
    fn rejects_bad_section_magic() {
        let mut data = sample_manifest();
        data[..4].copy_from_slice(&0xdeadbeefu32.to_le_bytes());
        let err = parse_manifest(&data).unwrap_err().to_string();
        assert!(err.contains("bad payload magic"), "{err}");
    }

    #[test]
    fn rejects_truncated_sections() {
        let data = sample_manifest();
        for end in [0, 4, 7, data.len() - 1] {
            assert!(parse_manifest(&data[..end]).is_err());
        }
    }

    #[test]
    fn extracts_first_zip_entry() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("manifest", options).unwrap();
            writer.write_all(b"manifest bytes").unwrap();
            writer.finish().unwrap();
        }

        assert_eq!(extract_zip(buf.get_ref()).unwrap(), b"manifest bytes");
    }

    #[test]
    fn rejects_empty_and_invalid_zips() {
        assert!(extract_zip(b"").is_err());
        assert!(extract_zip(b"PK\x03\x04 not really a zip").is_err());
    }

    #[test]
    fn zlib_roundtrip() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"hello hello hello").unwrap();
        let compressed = encoder.finish().unwrap();

        assert_eq!(zlib_decompress(&compressed).unwrap(), b"hello hello hello");
        assert!(zlib_decompress(b"not zlib").is_err());
    }

    #[test]
    fn cdn_pool_prefers_lowest_penalty_and_rotates() {
        let mut pool = CdnPool::new(vec![server("a"), server("b"), server("c")]);
        assert_eq!(pool.pick_server().host, "a");

        pool.penalize("a");
        assert_eq!(pool.pick_server().host, "b");

        pool.penalize("b");
        pool.penalize("b");
        assert_eq!(pool.pick_server().host, "c");
    }

    #[test]
    fn cdn_auth_tokens_expire() {
        let mut pool = CdnPool::new(vec![server("a")]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;

        pool.set_cdn_auth_token(
            1,
            "a",
            CdnAuthToken {
                token: "fresh".into(),
                expiration: now + 300,
            },
        );
        pool.set_cdn_auth_token(
            2,
            "a",
            CdnAuthToken {
                token: "stale".into(),
                expiration: now.saturating_sub(1),
            },
        );

        assert_eq!(
            pool.get_cdn_auth_token(1, "a").map(|t| t.token.as_str()),
            Some("fresh")
        );
        assert!(pool.get_cdn_auth_token(2, "a").is_none());
        assert!(pool.get_cdn_auth_token(1, "missing").is_none());
    }

    #[test]
    fn builds_server_urls() {
        let mut srv = server("a");
        assert_eq!(
            srv.url("/depot/1/manifest/2/5/3"),
            "https://a.example.com/depot/1/manifest/2/5/3"
        );
        srv.scheme = "http".into();
        assert_eq!(srv.url("/x"), "http://a.example.com/x");
    }
}
