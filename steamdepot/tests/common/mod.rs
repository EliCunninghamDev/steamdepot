//! Shared test fixtures: a scripted mock Steam CM (WebSocket) and a tiny
//! static HTTP server for CDN responses.
//!
//! Everything here is offline and deterministic. No Steam credentials, no
//! network access, no TLS certificates.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit};
use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use steamdepot::emsg::EMsg;
use steamdepot::proto::{
    CContentServerDirectoryGetManifestRequestCodeResponse,
    CContentServerDirectoryGetServersForSteamPipeResponse, CContentServerDirectoryServerInfo,
    CMsgClientGetDepotDecryptionKeyResponse, CMsgClientLogonResponse,
    CMsgClientPicsAccessTokenResponse, CMsgClientPicsProductInfoResponse, CMsgProtoBufHeader,
    ContentManifestMetadata, ContentManifestPayload, ContentManifestSignature,
    c_msg_client_pics_access_token_response, c_msg_client_pics_product_info_response,
    content_manifest_payload,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const PROTO_MASK: u32 = 0x8000_0000;

/// Product-info KeyValues served by the mock CM for app `TEST_APP_ID`.
pub const TEST_APP_ID: u32 = 100;
pub const TEST_DEPOT_ID: u32 = 101;
pub const TEST_MANIFEST_ID: u64 = 1111;
pub const TEST_DEPOT_KEY: [u8; 32] = [0x11; 32];
pub const TEST_STEAM_ID: u64 = (1 << 56) | (10 << 52);
pub const TEST_SESSION_ID: i32 = 77;
pub const TEST_CELL_ID: u32 = 42;
pub const TEST_HEARTBEAT_SECONDS: i32 = 30;

pub const APPINFO: &str = r#"
"appinfo"
{
    "appid" "100"
    "common" { "name" "Mock Game" }
    "depots"
    {
        "101"
        {
            "config" { "oslist" "linux, windows, macos" }
            "manifests" { "public" "1111" }
        }
    }
}
"#;

// ---------------------------------------------------------------------------
// Mock CM
// ---------------------------------------------------------------------------

/// A mock CM reachable at `url` (`ws://127.0.0.1:<port>/cmsocket/`).
pub struct MockCm {
    pub url: String,
}

impl MockCm {
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            serve_connection(ws).await;
        });

        Self {
            url: format!("ws://{addr}/cmsocket/"),
        }
    }
}

async fn serve_connection<S>(ws: tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = ws.split();
    let mut steam_id = 0u64;
    let mut session_id = 0i32;

    while let Some(Ok(message)) = stream.next().await {
        let WsMessage::Binary(data) = message else {
            continue;
        };
        let (emsg, header, body) = parse_request(&data);

        if emsg == EMsg::ClientLogon as u32 {
            steam_id = header.steamid.unwrap_or(0);
            session_id = TEST_SESSION_ID;
            let response = CMsgClientLogonResponse {
                eresult: Some(1),
                heartbeat_seconds: Some(TEST_HEARTBEAT_SECONDS),
                cell_id: Some(TEST_CELL_ID),
                ..Default::default()
            };
            let mut reply = proto_header_echo(&header);
            reply.steamid = Some(steam_id);
            reply.client_sessionid = Some(session_id);
            send(&mut sink, EMsg::ClientLogOnResponse, &reply, &response).await;
        } else if emsg == EMsg::ClientPICSAccessTokenRequest as u32 {
            let response = CMsgClientPicsAccessTokenResponse {
                app_access_tokens: vec![c_msg_client_pics_access_token_response::AppToken {
                    appid: Some(TEST_APP_ID),
                    access_token: Some(0xdead_beef),
                }],
                ..Default::default()
            };
            send(
                &mut sink,
                EMsg::ClientPICSAccessTokenResponse,
                &session_header(steam_id, session_id),
                &response,
            )
            .await;
        } else if emsg == EMsg::ClientPICSProductInfoRequest as u32 {
            let response = CMsgClientPicsProductInfoResponse {
                apps: vec![c_msg_client_pics_product_info_response::AppInfo {
                    appid: Some(TEST_APP_ID),
                    change_number: Some(1),
                    buffer: Some(APPINFO.as_bytes().to_vec()),
                    ..Default::default()
                }],
                response_pending: Some(false),
                ..Default::default()
            };
            send(
                &mut sink,
                EMsg::ClientPICSProductInfoResponse,
                &session_header(steam_id, session_id),
                &response,
            )
            .await;
        } else if emsg == EMsg::ClientGetDepotDecryptionKey as u32 {
            let response = CMsgClientGetDepotDecryptionKeyResponse {
                eresult: Some(1),
                depot_id: Some(TEST_DEPOT_ID),
                depot_encryption_key: Some(TEST_DEPOT_KEY.to_vec()),
            };
            send(
                &mut sink,
                EMsg::ClientGetDepotDecryptionKeyResponse,
                &session_header(steam_id, session_id),
                &response,
            )
            .await;
        } else if emsg == EMsg::ServiceMethodCallFromClient as u32 {
            let method = header.target_job_name.as_deref().unwrap_or_default();
            let mut reply = proto_header_echo(&header);
            reply.steamid = Some(steam_id);
            reply.client_sessionid = Some(session_id);
            reply.eresult = Some(1);

            let (emsg_out, body_out) = match method {
                "ContentServerDirectory.GetServersForSteamPipe#1" => (
                    EMsg::ServiceMethodResponse,
                    CContentServerDirectoryGetServersForSteamPipeResponse {
                        servers: vec![CContentServerDirectoryServerInfo {
                            r#type: Some("CDN".into()),
                            host: Some("mock.cdn".into()),
                            vhost: Some("mock.cdn".into()),
                            load: Some(0),
                            cell_id: Some(TEST_CELL_ID as i32),
                            ..Default::default()
                        }],
                        no_change: Some(false),
                    }
                    .encode_to_vec(),
                ),
                "ContentServerDirectory.GetManifestRequestCode#1" => (
                    EMsg::ServiceMethodResponse,
                    CContentServerDirectoryGetManifestRequestCodeResponse {
                        manifest_request_code: Some(999),
                    }
                    .encode_to_vec(),
                ),
                "Test.Fail#1" => {
                    reply.eresult = Some(15);
                    (EMsg::ServiceMethodResponse, Vec::new())
                }
                _ => {
                    // Echo the request body so callers can round-trip data.
                    (EMsg::ServiceMethodResponse, body)
                }
            };

            send_bytes(&mut sink, emsg_out, &reply, &body_out).await;
        } else if emsg == EMsg::ClientLogOff as u32 {
            break;
        }
    }
}

fn parse_request(data: &[u8]) -> (u32, CMsgProtoBufHeader, Vec<u8>) {
    let raw = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let emsg = raw & !PROTO_MASK;
    let header_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let header = CMsgProtoBufHeader::decode(&data[8..8 + header_len]).unwrap();
    let body = data[8 + header_len..].to_vec();
    (emsg, header, body)
}

fn proto_header_echo(request: &CMsgProtoBufHeader) -> CMsgProtoBufHeader {
    CMsgProtoBufHeader {
        jobid_target: request.jobid_source,
        steamid: request.steamid,
        client_sessionid: request.client_sessionid,
        ..Default::default()
    }
}

fn session_header(steam_id: u64, session_id: i32) -> CMsgProtoBufHeader {
    CMsgProtoBufHeader {
        steamid: Some(steam_id),
        client_sessionid: Some(session_id),
        ..Default::default()
    }
}

async fn send<S>(
    sink: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    emsg: EMsg,
    header: &CMsgProtoBufHeader,
    body: &impl Message,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_bytes(sink, emsg, header, &body.encode_to_vec()).await;
}

async fn send_bytes<S>(
    sink: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
    emsg: EMsg,
    header: &CMsgProtoBufHeader,
    body: &[u8],
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let header = header.encode_to_vec();
    let mut frame = Vec::with_capacity(8 + header.len() + body.len());
    frame.extend_from_slice(&(emsg as u32 | PROTO_MASK).to_le_bytes());
    frame.extend_from_slice(&(header.len() as u32).to_le_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(body);
    let _ = sink.send(WsMessage::Binary(frame.into())).await;
}

// ---------------------------------------------------------------------------
// Mock CDN (HTTP)
// ---------------------------------------------------------------------------

pub struct MockHttp {
    pub addr: SocketAddr,
}

impl MockHttp {
    /// Serve fixed responses keyed by request path (query strings ignored).
    pub async fn spawn(routes: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: HashMap<String, Vec<u8>> = routes.into_iter().collect();
        let routes = Arc::new(routes);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let routes = Arc::clone(&routes);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = vec![0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let path = path.split('?').next().unwrap_or(path);

                    match routes.get(path) {
                        Some(body) => {
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = socket.write_all(head.as_bytes()).await;
                            let _ = socket.write_all(body).await;
                        }
                        None => {
                            let _ = socket
                                .write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                )
                                .await;
                        }
                    }
                });
            }
        });

        Self { addr }
    }
}

// ---------------------------------------------------------------------------
// Manifest and chunk fixtures
// ---------------------------------------------------------------------------

const PAYLOAD_MAGIC: u32 = 0x71F6_17D0;
const METADATA_MAGIC: u32 = 0x1F48_12BE;
const SIGNATURE_MAGIC: u32 = 0x1B81_B817;

fn section(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&magic.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Build the raw (uncompressed) manifest blob.
pub fn manifest_blob(
    mappings: Vec<content_manifest_payload::FileMapping>,
    metadata: ContentManifestMetadata,
) -> Vec<u8> {
    let payload = ContentManifestPayload { mappings };
    let signature = ContentManifestSignature::default();

    [
        section(PAYLOAD_MAGIC, &payload.encode_to_vec()),
        section(METADATA_MAGIC, &metadata.encode_to_vec()),
        section(SIGNATURE_MAGIC, &signature.encode_to_vec()),
    ]
    .concat()
}

/// Wrap bytes in a single-entry ZIP archive, which is how the CDN serves
/// manifests.
pub fn zip_blob(name: &str, data: &[u8]) -> Vec<u8> {
    use std::io::Write;

    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(name, options).unwrap();
        writer.write_all(data).unwrap();
        writer.finish().unwrap();
    }
    buf.into_inner()
}

/// Encrypt plaintext the way Steam serves chunks: an ECB-encrypted IV followed
/// by AES-256-CBC ciphertext with PKCS7 padding.
pub fn encrypt_chunk(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new_from_slice(key).unwrap();
    let mut iv_ct = *iv;
    cipher.encrypt_block(aes::Block::from_mut_slice(&mut iv_ct));

    let encryptor = cbc::Encryptor::<Aes256>::new_from_slices(key, iv).unwrap();
    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let len = encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .unwrap()
        .len();
    buf.truncate(len);

    [iv_ct.as_slice(), buf.as_slice()].concat()
}

/// Steam's Adler-32 variant (initial seed 0).
pub fn adler32_steam(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut s1: u32 = 0;
    let mut s2: u32 = 0;
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            s1 = s1.wrapping_add(byte as u32);
            s2 = s2.wrapping_add(s1);
        }
        s1 %= MOD_ADLER;
        s2 %= MOD_ADLER;
    }
    (s2 << 16) | s1
}

pub fn sha_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
