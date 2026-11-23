//! Offline end-to-end tests for manifest fetching, decryption, directory
//! preparation, and chunk downloads against a local HTTP "CDN".

mod common;

use base64::Engine;
use common::*;
use std::sync::{Arc, Mutex};
use steamdepot::cdn::{CdnPool, CdnServer};
use steamdepot::download;
use steamdepot::proto::{ContentManifestMetadata, content_manifest_payload};

use content_manifest_payload::FileMapping;
use content_manifest_payload::file_mapping::ChunkData;

const CHUNK_IV: [u8; 16] = [0x5a; 16];

fn test_server(addr: std::net::SocketAddr) -> CdnServer {
    CdnServer {
        host: addr.ip().to_string(),
        vhost: addr.to_string(),
        server_type: "CDN".into(),
        load: 0,
        cell_id: 0,
        scheme: "http".into(),
    }
}

/// Build a one-file manifest plus the chunk fixtures needed to serve it.
struct Fixture {
    manifest: Vec<u8>,
    chunk: Vec<u8>,
    plaintext: Vec<u8>,
    sha_hex: String,
}

fn fixture(encrypted_filenames: bool, tamper_crc: bool) -> Fixture {
    let plaintext = b"hello from the mock CDN\n".repeat(64);
    let sha = [0xab; 20];
    let encrypted = encrypt_chunk(&TEST_DEPOT_KEY, &CHUNK_IV, &plaintext);

    let filename = if encrypted_filenames {
        let encrypted_name = encrypt_chunk(&TEST_DEPOT_KEY, &CHUNK_IV, b"bin/server");
        base64::engine::general_purpose::STANDARD.encode(encrypted_name)
    } else {
        "bin/server".to_string()
    };

    let mut crc = adler32_steam(&plaintext);
    if tamper_crc {
        crc ^= 0xffff_ffff;
    }

    let mapping = FileMapping {
        filename: Some(filename),
        size: Some(plaintext.len() as u64),
        flags: Some(0),
        chunks: vec![ChunkData {
            sha: Some(sha.to_vec()),
            crc: Some(crc),
            offset: Some(0),
            cb_original: Some(plaintext.len() as u32),
            cb_compressed: Some(encrypted.len() as u32),
        }],
        ..Default::default()
    };

    let metadata = ContentManifestMetadata {
        depot_id: Some(TEST_DEPOT_ID),
        gid_manifest: Some(TEST_MANIFEST_ID),
        filenames_encrypted: Some(encrypted_filenames),
        cb_disk_original: Some(plaintext.len() as u64),
        cb_disk_compressed: Some(encrypted.len() as u64),
        unique_chunks: Some(1),
        ..Default::default()
    };

    Fixture {
        manifest: zip_blob("manifest", &manifest_blob(vec![mapping], metadata)),
        chunk: encrypted,
        plaintext,
        sha_hex: sha_hex(&sha),
    }
}

#[tokio::test]
async fn downloads_manifest_chunks_and_writes_the_file() {
    let fx = fixture(true, false);
    let http = MockHttp::spawn(vec![
        (
            format!("/depot/{TEST_DEPOT_ID}/manifest/{TEST_MANIFEST_ID}/5/999"),
            fx.manifest.clone(),
        ),
        (
            format!("/depot/{TEST_DEPOT_ID}/chunk/{}", fx.sha_hex),
            fx.chunk.clone(),
        ),
    ])
    .await;

    let server = test_server(http.addr);
    let client = reqwest::Client::new();

    let mut manifest = steamdepot::cdn::download_manifest(
        &client,
        &server,
        TEST_DEPOT_ID,
        TEST_MANIFEST_ID,
        999,
        &TEST_DEPOT_KEY,
        None,
    )
    .await
    .unwrap();

    assert_eq!(manifest.metadata.gid_manifest, Some(TEST_MANIFEST_ID));

    download::decrypt_manifest_filenames(&mut manifest, &TEST_DEPOT_KEY).unwrap();
    assert_eq!(
        manifest.payload.mappings[0].filename.as_deref(),
        Some("bin/server")
    );

    let dir = tempfile::tempdir().unwrap();
    let prepared = download::prepare_directory_tree(dir.path(), &manifest)
        .await
        .unwrap();
    assert_eq!(prepared.files_created, 1);
    assert_eq!(prepared.total_bytes, fx.plaintext.len() as u64);

    let progress: Arc<Mutex<Vec<download::DownloadProgress>>> = Arc::default();
    let pool = Arc::new(Mutex::new(CdnPool::new(vec![server])));
    let result = download::download_depot(
        &client,
        pool,
        TEST_DEPOT_ID,
        &manifest,
        &TEST_DEPOT_KEY,
        dir.path(),
        4,
        {
            let progress = Arc::clone(&progress);
            move |p| progress.lock().unwrap().push(p.clone())
        },
    )
    .await
    .unwrap();

    assert_eq!(result.chunks_total, 1);
    assert_eq!(result.chunks_done, 1);
    assert_eq!(result.bytes_downloaded, fx.chunk.len() as u64);
    assert_eq!(progress.lock().unwrap().len(), 1);

    let written = tokio::fs::read(dir.path().join("bin/server"))
        .await
        .unwrap();
    assert_eq!(written, fx.plaintext);
}

#[tokio::test]
async fn rejects_chunks_with_the_wrong_checksum() {
    let fx = fixture(false, true);
    let http = MockHttp::spawn(vec![
        (
            format!("/depot/{TEST_DEPOT_ID}/manifest/{TEST_MANIFEST_ID}/5/999"),
            fx.manifest.clone(),
        ),
        (
            format!("/depot/{TEST_DEPOT_ID}/chunk/{}", fx.sha_hex),
            fx.chunk.clone(),
        ),
    ])
    .await;

    let server = test_server(http.addr);
    let client = reqwest::Client::new();
    let mut manifest = steamdepot::cdn::download_manifest(
        &client,
        &server,
        TEST_DEPOT_ID,
        TEST_MANIFEST_ID,
        999,
        &TEST_DEPOT_KEY,
        None,
    )
    .await
    .unwrap();
    download::decrypt_manifest_filenames(&mut manifest, &TEST_DEPOT_KEY).unwrap();

    let dir = tempfile::tempdir().unwrap();
    download::prepare_directory_tree(dir.path(), &manifest)
        .await
        .unwrap();

    let pool = Arc::new(Mutex::new(CdnPool::new(vec![server])));
    let err = download::download_depot(
        &client,
        pool,
        TEST_DEPOT_ID,
        &manifest,
        &TEST_DEPOT_KEY,
        dir.path(),
        2,
        |_| {},
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("Adler-32 mismatch"), "{err}");
}

#[tokio::test]
async fn reports_missing_cdn_objects() {
    let http = MockHttp::spawn(Vec::new()).await;
    let server = test_server(http.addr);
    let client = reqwest::Client::new();

    let err = steamdepot::cdn::download_manifest(
        &client,
        &server,
        TEST_DEPOT_ID,
        TEST_MANIFEST_ID,
        999,
        &TEST_DEPOT_KEY,
        None,
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("404"), "{err}");
}

#[tokio::test]
async fn prepare_directory_tree_creates_nested_parents() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = manifest_blob(
        vec![FileMapping {
            filename: Some("a/b/c/file.bin".into()),
            size: Some(0),
            ..Default::default()
        }],
        ContentManifestMetadata::default(),
    );

    // Reuse the CDN parser to keep the fixture format honest.
    let http = MockHttp::spawn(vec![(
        format!("/depot/{TEST_DEPOT_ID}/manifest/{TEST_MANIFEST_ID}/5/999"),
        zip_blob("manifest", &manifest),
    )])
    .await;
    let server = test_server(http.addr);
    let client = reqwest::Client::new();

    let manifest = steamdepot::cdn::download_manifest(
        &client,
        &server,
        TEST_DEPOT_ID,
        TEST_MANIFEST_ID,
        999,
        &TEST_DEPOT_KEY,
        None,
    )
    .await
    .unwrap();

    download::prepare_directory_tree(dir.path(), &manifest)
        .await
        .unwrap();

    assert!(dir.path().join("a/b/c/file.bin").exists());
}
