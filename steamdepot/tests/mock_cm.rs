//! Offline protocol tests against a scripted mock Steam CM.

mod common;

use common::*;
use steamdepot::cdn;
use steamdepot::connection::CmConnection;
use steamdepot::depot::{self, DownloadConfig};
use steamdepot::error::Error;
use steamdepot::login;
use steamdepot::pics;

async fn connect_and_login() -> CmConnection {
    let cm = MockCm::spawn().await;
    let mut conn = CmConnection::connect(&cm.url).await.unwrap();
    login::login_anonymous_with_id(&mut conn, 0x1234_5678)
        .await
        .unwrap();
    conn
}

#[tokio::test]
async fn anonymous_login_populates_session() {
    let mut conn = connect_and_login().await;

    let session = conn.session().expect("session set after login");
    assert_eq!(session.steam_id, TEST_STEAM_ID);
    assert_eq!(session.session_id, TEST_SESSION_ID);
    assert_eq!(session.cell_id, TEST_CELL_ID);
    assert_eq!(session.heartbeat_seconds, TEST_HEARTBEAT_SECONDS);
    assert!(!session.authenticated);

    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_login_marks_the_session_authenticated() {
    use base64::Engine;

    let payload = format!(r#"{{"iss":"steam","sub":"{}","exp":1}}"#, TEST_STEAM_ID);
    let payload = base64::engine::general_purpose::STANDARD.encode(payload);
    let token = format!("header.{payload}.signature");

    let cm = MockCm::spawn().await;
    let mut conn = CmConnection::connect(&cm.url).await.unwrap();
    let session = login::login_with_token(&mut conn, "alice", &token)
        .await
        .unwrap();

    assert!(session.authenticated);
    assert_eq!(session.steam_id, TEST_STEAM_ID);

    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn fetches_product_info_and_depot_key() {
    let mut conn = connect_and_login().await;

    let info = pics::get_product_info(&mut conn, &[TEST_APP_ID], &[])
        .await
        .unwrap();
    assert_eq!(info.apps.len(), 1);
    assert_eq!(info.apps[0].appid, TEST_APP_ID);
    assert!(String::from_utf8_lossy(&info.apps[0].buffer).contains("\"appinfo\""));

    let key = pics::get_depot_decryption_key(&mut conn, TEST_DEPOT_ID, TEST_APP_ID)
        .await
        .unwrap();
    assert_eq!(key, TEST_DEPOT_KEY);

    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn prepares_a_download_plan() {
    let mut conn = connect_and_login().await;

    let config = DownloadConfig {
        app_id: TEST_APP_ID,
        os: "linux".into(),
        branch: "public".into(),
    };
    let plan = depot::prepare_download(&mut conn, &config).await.unwrap();

    assert_eq!(plan.app_id, TEST_APP_ID);
    assert_eq!(plan.plans.len(), 1);
    assert_eq!(plan.plans[0].depot.depot_id, TEST_DEPOT_ID);
    assert_eq!(plan.plans[0].depot.manifest_id, TEST_MANIFEST_ID);
    assert_eq!(plan.plans[0].key, TEST_DEPOT_KEY);

    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn fetches_cdn_servers_and_manifest_codes() {
    let mut conn = connect_and_login().await;

    let servers = cdn::get_cdn_servers(&mut conn, TEST_CELL_ID).await.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].host, "mock.cdn");
    assert_eq!(servers[0].scheme, "https");

    let code = cdn::get_manifest_request_code(
        &mut conn,
        TEST_APP_ID,
        TEST_DEPOT_ID,
        TEST_MANIFEST_ID,
        "public",
    )
    .await
    .unwrap();
    assert_eq!(code, 999);

    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn surfaces_service_method_eresults() {
    let mut conn = connect_and_login().await;

    let err = conn
        .service_method_call("Test.Fail#1", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::EResult { eresult: 15, .. }),
        "unexpected error: {err:?}"
    );

    conn.shutdown().await.unwrap();
}
