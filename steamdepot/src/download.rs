use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::cdn::{CdnPool, DepotManifest};
use crate::crypto;
use crate::error::{Error, Result};

#[cfg_attr(not(unix), allow(dead_code))] // only used to set the exec bit on Unix
const FLAG_EXECUTABLE: u32 = 0x20;
const FLAG_READONLY: u32 = 0x08;
const FLAG_DIRECTORY: u32 = 0x40;
const FLAG_SYMLINK: u32 = 0x200;

const MAX_RETRIES: u32 = 3;

/// Summary of what `prepare_directory_tree` created.
#[derive(Debug)]
pub struct PrepareResult {
    pub dirs_created: u64,
    pub files_created: u64,
    pub symlinks_created: u64,
    pub total_bytes: u64,
}

/// Decrypt all encrypted filenames in a manifest in-place.
///
/// If `metadata.filenames_encrypted` is false (or absent), this is a no-op.
pub fn decrypt_manifest_filenames(manifest: &mut DepotManifest, key: &[u8]) -> Result<()> {
    if !manifest.metadata.filenames_encrypted.unwrap_or(false) {
        return Ok(());
    }

    for mapping in &mut manifest.payload.mappings {
        if let Some(ref encrypted) = mapping.filename {
            let decrypted = crypto::decrypt_filename(key, encrypted)?;
            mapping.filename = Some(decrypted);
        }
    }

    manifest.metadata.filenames_encrypted = Some(false);
    Ok(())
}

/// Join a manifest filename onto the install directory.
///
/// Manifest filenames are attacker-controlled data (they come from Steam, but
/// also from any mirror). Absolute paths, drive prefixes, and `..` components
/// are rejected so a manifest cannot write outside the install directory.
fn safe_join(root: &Path, filename: &str) -> Result<PathBuf> {
    let normalized = filename.replace('\\', "/");
    let mut out = root.to_path_buf();
    let mut pushed = false;

    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                pushed = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Other(format!(
                    "unsafe path in manifest: {filename:?}"
                )));
            }
        }
    }

    if !pushed {
        return Err(Error::Other(format!(
            "empty path in manifest: {filename:?}"
        )));
    }

    Ok(out)
}

/// Create the directory tree and pre-allocate files at their final sizes.
///
/// Handles directories (flag 0x40), symlinks (flag 0x200), and regular files.
/// Regular file modes are derived from the manifest flags: executable (0x20)
/// files become `0755`, read-only (0x08) files lose their write bits on Unix
/// (see `apply_file_flags`). Read-only is deferred until the chunks are on disk.
pub async fn prepare_directory_tree(
    install_dir: &Path,
    manifest: &DepotManifest,
) -> Result<PrepareResult> {
    let mut result = PrepareResult {
        dirs_created: 0,
        files_created: 0,
        symlinks_created: 0,
        total_bytes: 0,
    };

    // Sort mappings by filename for deterministic creation order
    let mut mappings: Vec<_> = manifest.payload.mappings.iter().collect();
    mappings.sort_by(|a, b| {
        let a_name = a.filename.as_deref().unwrap_or("");
        let b_name = b.filename.as_deref().unwrap_or("");
        a_name.cmp(b_name)
    });

    for mapping in &mappings {
        let filename = match mapping.filename.as_deref() {
            Some(f) => f,
            None => continue,
        };
        let flags = mapping.flags.unwrap_or(0);
        let size = mapping.size.unwrap_or(0);

        let path = safe_join(install_dir, filename)?;

        if flags & FLAG_DIRECTORY != 0 {
            tokio::fs::create_dir_all(&path).await?;
            result.dirs_created += 1;
        } else if flags & FLAG_SYMLINK != 0 {
            let target = mapping.linktarget.as_deref().unwrap_or("");
            if target.is_empty() {
                return Err(Error::Other(format!(
                    "symlink {} has no linktarget",
                    filename
                )));
            }
            // Ensure parent dir exists
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            // Remove existing file/symlink if present
            let _ = tokio::fs::remove_file(&path).await;
            tokio::fs::symlink(target, &path).await?;
            result.symlinks_created += 1;
        } else {
            // Regular file
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let file = match tokio::fs::File::create(&path).await {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    // A read-only file from an earlier download has to be made
                    // writable before it can be truncated.
                    make_writable(&path).await?;
                    tokio::fs::File::create(&path).await?
                }
                Err(e) => return Err(e.into()),
            };
            file.set_len(size).await?;
            result.files_created += 1;
            result.total_bytes += size;

            // Apply the manifest mode now so stale bits are cleared. Files that
            // still need chunks stay writable; `download_depot` reapplies the
            // full mode after the last chunk is written.
            let flags = if mapping.chunks.is_empty() {
                flags
            } else {
                flags & !FLAG_READONLY
            };
            apply_file_flags(&path, flags).await?;
        }
    }

    Ok(result)
}

/// Ensure an existing file can be opened for writing.
///
/// Files marked read-only in a previous download are `0444` on Unix (and carry
/// the read-only attribute on Windows), which makes truncating them fail.
async fn make_writable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = tokio::fs::metadata(path).await?.permissions();
        if perms.mode() & 0o200 == 0 {
            let mode = perms.mode() | 0o200;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
        }
    }

    #[cfg(windows)]
    {
        let mut perms = tokio::fs::metadata(path).await?.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            tokio::fs::set_permissions(path, perms).await?;
        }
    }

    #[cfg(not(any(unix, windows)))]
    let _ = path;

    Ok(())
}

/// Rewrite a regular file's mode from its manifest flags.
///
/// The mode is derived from scratch on every download, so a file that loses
/// the executable (0x20) or read-only (0x08) flag in a newer manifest does not
/// keep a stale permission bit.
///
/// On Unix the result is `0644` (`0755` when executable), with every write bit
/// cleared for read-only files. The hidden flag (0x10) only maps to a Windows
/// file attribute that `std` cannot set, so it is ignored on every platform.
async fn apply_file_flags(path: &Path, flags: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut mode = 0o644;
        if flags & FLAG_EXECUTABLE != 0 {
            mode |= 0o111;
        }
        if flags & FLAG_READONLY != 0 {
            mode &= !0o222;
        }
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    }

    #[cfg(windows)]
    {
        let read_only = flags & FLAG_READONLY != 0;
        let mut perms = tokio::fs::metadata(path).await?.permissions();
        if perms.readonly() != read_only {
            perms.set_readonly(read_only);
            tokio::fs::set_permissions(path, perms).await?;
        }
    }

    #[cfg(not(any(unix, windows)))]
    let _ = (path, flags);

    Ok(())
}

/// Progress report for chunk downloads.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub chunks_done: u64,
    pub chunks_total: u64,
    pub bytes_downloaded: u64,
}

/// A single chunk download job.
struct ChunkJob {
    path: PathBuf,
    sha_hex: String,
    crc: u32,
    offset: u64,
}

/// Download all chunks for a depot concurrently, rotating CDN servers on failure.
///
/// Flattens all chunks across all files into a work queue and processes them
/// with bounded concurrency. Each chunk is retried up to `MAX_RETRIES` times,
/// penalizing the failing server in the pool on transient errors.
#[allow(clippy::too_many_arguments)] // public entry point; splitting would churn the API
pub async fn download_depot(
    client: &reqwest::Client,
    pool: Arc<Mutex<CdnPool>>,
    depot_id: u32,
    manifest: &DepotManifest,
    depot_key: &[u8],
    install_dir: &Path,
    max_concurrent: usize,
    on_progress: impl Fn(&DownloadProgress) + Send + Sync + 'static,
) -> Result<DownloadProgress> {
    // Flatten all chunks into a work queue
    let mut jobs = Vec::new();
    for mapping in &manifest.payload.mappings {
        let flags = mapping.flags.unwrap_or(0);
        if flags & FLAG_DIRECTORY != 0 || flags & FLAG_SYMLINK != 0 {
            continue;
        }
        let filename = match mapping.filename.as_deref() {
            Some(f) => f,
            None => continue,
        };
        if mapping.chunks.is_empty() {
            continue;
        }

        let path = safe_join(install_dir, filename)?;

        for chunk in &mapping.chunks {
            let sha_bytes = chunk.sha.as_deref().unwrap_or(&[]);
            let sha_hex: String = sha_bytes.iter().map(|b| format!("{:02x}", b)).collect();
            jobs.push(ChunkJob {
                path: path.clone(),
                sha_hex,
                crc: chunk.crc.unwrap_or(0),
                offset: chunk.offset.unwrap_or(0),
            });
        }
    }

    let chunks_total = jobs.len() as u64;
    let chunks_done = Arc::new(AtomicU64::new(0));
    let bytes_downloaded = Arc::new(AtomicU64::new(0));
    let on_progress = Arc::new(on_progress);

    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let mut join_set = JoinSet::new();

    for job in jobs {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let pool = pool.clone();
        let depot_key = depot_key.to_vec();
        let chunks_done = chunks_done.clone();
        let bytes_downloaded = bytes_downloaded.clone();
        let on_progress = on_progress.clone();

        join_set.spawn(async move {
            let result =
                download_chunk_with_retry(&client, &pool, depot_id, &job, &depot_key).await;
            drop(permit);

            match result {
                Ok(chunk_bytes) => {
                    bytes_downloaded.fetch_add(chunk_bytes, Ordering::Relaxed);
                    chunks_done.fetch_add(1, Ordering::Relaxed);
                    on_progress(&DownloadProgress {
                        chunks_done: chunks_done.load(Ordering::Relaxed),
                        chunks_total,
                        bytes_downloaded: bytes_downloaded.load(Ordering::Relaxed),
                    });
                    Ok(())
                }
                Err(e) => Err(e),
            }
        });
    }

    // Collect results, fail on first error
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                join_set.abort_all();
                return Err(e);
            }
            Err(e) => {
                join_set.abort_all();
                return Err(Error::Other(format!("chunk task panicked: {}", e)));
            }
        }
    }

    // Now that every chunk is on disk, apply the final mode from the manifest
    // (read-only files were kept writable until this point).
    for mapping in &manifest.payload.mappings {
        let flags = mapping.flags.unwrap_or(0);
        if flags & (FLAG_DIRECTORY | FLAG_SYMLINK) != 0 {
            continue;
        }
        if let Some(filename) = mapping.filename.as_deref() {
            let path = safe_join(install_dir, filename)?;
            apply_file_flags(&path, flags).await?;
        }
    }

    Ok(DownloadProgress {
        chunks_done: chunks_done.load(Ordering::Relaxed),
        chunks_total,
        bytes_downloaded: bytes_downloaded.load(Ordering::Relaxed),
    })
}

/// Download, decrypt, decompress, verify, and write a single chunk with retries.
///
/// Returns the number of encrypted bytes downloaded on success.
async fn download_chunk_with_retry(
    client: &reqwest::Client,
    pool: &Arc<Mutex<CdnPool>>,
    depot_id: u32,
    job: &ChunkJob,
    depot_key: &[u8],
) -> Result<u64> {
    for attempt in 0..=MAX_RETRIES {
        let (server, cdn_token) = {
            let pool = pool.lock().unwrap();
            let server = pool.pick_server().clone();
            let token = pool
                .get_cdn_auth_token(depot_id, &server.host)
                .map(|t| t.token.clone());
            (server, token)
        };
        let mut url = server.url(&format!("/depot/{}/chunk/{}", depot_id, job.sha_hex));
        if let Some(ref token) = cdn_token {
            url.push_str(token);
        }

        match fetch_and_process(client, &url, depot_key, job).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if is_retryable(&e) && attempt < MAX_RETRIES => {
                pool.lock().unwrap().penalize(&server.host);
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Fetch a chunk from CDN, decrypt, decompress, verify CRC, and write to file.
///
/// Returns the number of encrypted bytes downloaded.
async fn fetch_and_process(
    client: &reqwest::Client,
    url: &str,
    depot_key: &[u8],
    job: &ChunkJob,
) -> Result<u64> {
    let resp = client
        .get(url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| Error::Other(format!("chunk download {}: {}", job.sha_hex, e)))?;
    let encrypted = resp.bytes().await?;
    let encrypted_len = encrypted.len() as u64;

    // Decrypt
    let decrypted = crypto::symmetric_decrypt(depot_key, &encrypted)?;

    // Decompress
    let decompressed = decompress_chunk(&decrypted)?;

    // Verify Adler-32
    verify_adler32(&decompressed, job.crc)?;

    // Write at correct offset using spawn_blocking (sync file I/O)
    let path = job.path.clone();
    let offset = job.offset;
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| Error::Other(format!("open {}: {}", path.display(), e)))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&decompressed)?;
        Ok::<(), Error>(())
    })
    .await
    .map_err(|e| Error::Other(format!("write task panicked: {}", e)))??;

    Ok(encrypted_len)
}

/// Check if an error is retryable (transient HTTP or network failures).
fn is_retryable(e: &Error) -> bool {
    match e {
        Error::Http(re) => {
            if let Some(status) = re.status() {
                matches!(status.as_u16(), 403 | 404 | 500 | 502 | 503 | 429)
            } else {
                // Connection/timeout errors
                re.is_connect() || re.is_timeout()
            }
        }
        Error::Other(msg) => {
            // Retryable HTTP status errors surfaced via error_for_status
            msg.contains("403")
                || msg.contains("500")
                || msg.contains("502")
                || msg.contains("503")
                || msg.contains("404")
                || msg.contains("429")
        }
        _ => false,
    }
}

/// Detect compression format from magic bytes and decompress.
///
/// SteamKit2 DepotChunk.cs checks for these magic prefixes after decryption:
/// - `VSZa` (4 bytes) → Zstd with VZstd envelope
/// - `VZa` (3 bytes) → LZMA with VZip envelope
/// - `PK\x03\x04` → ZIP
/// - Otherwise → uncompressed
fn decompress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() >= 4 && &data[..4] == b"VSZa" {
        decompress_vzstd(data)
    } else if data.len() >= 3 && &data[..3] == b"VZa" {
        decompress_vzip_lzma(data)
    } else if data.len() >= 4 && &data[..4] == b"PK\x03\x04" {
        // ZIP-compressed chunk (rare)
        let cursor = std::io::Cursor::new(data);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| Error::Other(format!("chunk zip: {}", e)))?;
        let mut file = archive
            .by_index(0)
            .map_err(|e| Error::Other(format!("chunk zip entry: {}", e)))?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf)?;
        Ok(buf)
    } else {
        // Uncompressed
        Ok(data.to_vec())
    }
}

/// Decompress a VZstd chunk.
///
/// `VZstdUtil.cs` layout:
///
/// ```text
/// [0..4]   'V' 'S' 'Z' 'a' magic
/// [4..8]   CRC32
/// [8..-15] Zstd compressed stream
/// [-15..]  footer (CRC32 + decompressed size + "zsv")
/// ```
fn decompress_vzstd(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 8 + 15 {
        return Err(Error::Other("VZstd data too short".into()));
    }
    let zstd_data = &data[8..data.len() - 15];
    let mut decoder = ruzstd::StreamingDecoder::new(zstd_data)
        .map_err(|e| Error::Other(format!("VZstd init: {}", e)))?;
    let mut output = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut output)?;
    Ok(output)
}

/// Decompress a VZip (LZMA) chunk.
///
/// `VZipUtil.cs` layout:
///
/// ```text
/// [0..2]   'V' 'Z' magic
/// [2]      'a' version
/// [3..7]   CRC32 / timestamp
/// [7..12]  LZMA properties (1 byte props + 4 byte dict size)
/// [12..-10] LZMA compressed data
/// [-10..]  footer (CRC32 + decompressed size + "vz")
/// ```
fn decompress_vzip_lzma(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 12 + 10 {
        return Err(Error::Other("VZip data too short".into()));
    }

    // lzma-rs expects a standard LZMA header: 5 property bytes + 8-byte little-endian size.
    // We have the 5 property bytes at data[7..12]. For the size, we can read it from
    // the footer or pass -1 (unknown).
    let props = &data[7..12];
    let compressed = &data[12..data.len() - 10];

    // Read decompressed size from footer: 4 bytes LE at offset -6 (before "vz" marker)
    let footer = &data[data.len() - 10..];
    let decompressed_size = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]) as u64;

    // Build standard LZMA header: 5 props + 8 byte LE size
    let mut lzma_stream = Vec::with_capacity(13 + compressed.len());
    lzma_stream.extend_from_slice(props);
    lzma_stream.extend_from_slice(&decompressed_size.to_le_bytes());
    lzma_stream.extend_from_slice(compressed);

    let mut output = Vec::new();
    lzma_rs::lzma_decompress(&mut std::io::Cursor::new(&lzma_stream), &mut output)
        .map_err(|e| Error::Other(format!("VZip LZMA decompress: {}", e)))?;

    Ok(output)
}

/// Verify Adler-32 checksum of decompressed chunk data.
///
/// Steam uses a non-standard Adler-32 with initial seed 0 instead of 1
/// (see SteamKit2's `Adler32.Calculate(0, data)`).
fn verify_adler32(data: &[u8], expected: u32) -> Result<()> {
    if expected == 0 {
        return Ok(());
    }
    let actual = adler32_steam(data);
    if actual != expected {
        return Err(Error::Other(format!(
            "Adler-32 mismatch: expected {:08x}, got {:08x}",
            expected, actual
        )));
    }
    Ok(())
}

/// Adler-32 with initial seed 0 (Steam's variant).
///
/// Standard Adler-32 starts with s1=1, s2=0. Steam starts with s1=0, s2=0.
fn adler32_steam(data: &[u8]) -> u32 {
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::proto::{
        ContentManifestMetadata, ContentManifestPayload, ContentManifestSignature,
        content_manifest_payload,
    };

    use super::*;

    fn manifest(mappings: Vec<content_manifest_payload::FileMapping>) -> DepotManifest {
        DepotManifest {
            payload: ContentManifestPayload { mappings },
            metadata: ContentManifestMetadata::default(),
            signature: ContentManifestSignature::default(),
        }
    }

    fn file(name: &str, size: u64, flags: u32) -> content_manifest_payload::FileMapping {
        content_manifest_payload::FileMapping {
            filename: Some(name.into()),
            size: Some(size),
            flags: Some(flags),
            ..Default::default()
        }
    }

    #[test]
    fn safe_join_rejects_escape_attempts() {
        let root = Path::new("/tmp/install");
        assert_eq!(
            safe_join(root, "a\\b/c.txt").unwrap(),
            Path::new("/tmp/install/a/b/c.txt")
        );
        assert_eq!(
            safe_join(root, "./a/./b").unwrap(),
            Path::new("/tmp/install/a/b")
        );

        for bad in [
            "../escape",
            "a/../../escape",
            "/etc/passwd",
            "..\\escape",
            "",
        ] {
            assert!(
                safe_join(root, bad).is_err(),
                "{bad:?} should have been rejected"
            );
        }
    }

    #[test]
    fn adler32_matches_steam_seed_variant() {
        assert_eq!(adler32_steam(b""), 0);
        assert_eq!(adler32_steam(b"abc"), 0x024A_0126);
        assert!(verify_adler32(b"abc", 0).is_ok());
        assert!(verify_adler32(b"abc", 0x024A_0126).is_ok());
        assert!(verify_adler32(b"abc", 0x1234_5678).is_err());
    }

    #[test]
    fn decompresses_uncompressed_and_zipped_chunks() {
        assert_eq!(decompress_chunk(b"plain bytes").unwrap(), b"plain bytes");

        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("chunk", options).unwrap();
            writer.write_all(b"zipped bytes").unwrap();
            writer.finish().unwrap();
        }
        assert_eq!(decompress_chunk(buf.get_ref()).unwrap(), b"zipped bytes");
    }

    #[test]
    fn rejects_truncated_compressed_chunks() {
        assert!(decompress_chunk(b"VSZa").is_err());
        assert!(decompress_chunk(b"VZa").is_err());
        assert!(decompress_chunk(b"PK\x03\x04 broken").is_err());
    }

    #[tokio::test]
    async fn prepares_directories_files_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();

        let mappings = vec![
            file("data", 0, FLAG_DIRECTORY),
            file("data/bin/server", 4, FLAG_EXECUTABLE),
        ];

        #[cfg(unix)]
        let mappings = {
            let mut mappings = mappings;
            let mut link = file("data/link", 0, FLAG_SYMLINK);
            link.linktarget = Some("bin/server".into());
            mappings.push(link);
            mappings
        };

        let result = prepare_directory_tree(dir.path(), &manifest(mappings))
            .await
            .unwrap();
        assert_eq!(result.dirs_created, 1);
        assert_eq!(result.files_created, 1);
        assert_eq!(result.total_bytes, 4);

        #[cfg(unix)]
        assert_eq!(result.symlinks_created, 1);
        #[cfg(not(unix))]
        assert_eq!(result.symlinks_created, 0);

        let server = dir.path().join("data/bin/server");
        assert_eq!(std::fs::metadata(&server).unwrap().len(), 4);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&server).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                std::fs::read_link(dir.path().join("data/link")).unwrap(),
                Path::new("bin/server")
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn derives_file_modes_from_manifest_flags() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();

        // Simulate a file left executable by an older download.
        let stale = dir.path().join("stale");
        tokio::fs::write(&stale, b"x").await.unwrap();
        tokio::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();

        let manifest = manifest(vec![
            file("stale", 1, 0),
            file("tool", 0, FLAG_EXECUTABLE),
            file("locked", 0, FLAG_READONLY),
            file("locked_tool", 0, FLAG_EXECUTABLE | FLAG_READONLY),
        ]);
        prepare_directory_tree(dir.path(), &manifest).await.unwrap();

        let mode = |name: &str| {
            std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };

        assert_eq!(mode("stale"), 0o644, "stale exec bit was not cleared");
        assert_eq!(mode("tool"), 0o755);
        assert_eq!(mode("locked"), 0o444);
        assert_eq!(mode("locked_tool"), 0o555);
    }

    #[tokio::test]
    async fn re_prepares_read_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest(vec![file("locked", 2, FLAG_READONLY)]);

        prepare_directory_tree(dir.path(), &manifest).await.unwrap();
        let result = prepare_directory_tree(dir.path(), &manifest).await.unwrap();
        assert_eq!(result.files_created, 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("locked"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o444);
        }
        #[cfg(windows)]
        assert!(
            std::fs::metadata(dir.path().join("locked"))
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    #[tokio::test]
    async fn refuses_to_write_outside_the_install_directory() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest(vec![file("../escaped.txt", 1, 0)]);

        let err = prepare_directory_tree(dir.path(), &manifest)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsafe path"), "{err}");
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    }

    #[tokio::test]
    async fn requires_a_target_for_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest(vec![file("dangling", 0, FLAG_SYMLINK)]);

        let err = prepare_directory_tree(dir.path(), &manifest)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no linktarget"), "{err}");
    }
}
