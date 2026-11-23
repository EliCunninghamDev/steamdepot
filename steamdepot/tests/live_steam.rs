//! Tests that talk to real Steam infrastructure.
//!
//! These are `#[ignore]`d so `cargo test` stays offline and deterministic.
//! Run them with `scripts/test-live.sh`; see `.env.example` for configuration.

use steamdepot::connection::CmConnection;
use steamdepot::depot::{self, DownloadConfig};
use steamdepot::login;
use steamdepot::pics;
use steamdepot::steam_api::cm_list::{self, CmServerType};

fn test_app_id() -> u32 {
    std::env::var("STEAMDEPOT_TEST_APP_ID")
        .ok()
        .and_then(|value| value.parse().ok())
        // TF2 dedicated server: publicly downloadable anonymously.
        .unwrap_or(232250)
}

async fn anonymous_connection() -> CmConnection {
    let client = reqwest::Client::new();
    let list = cm_list::get_cm_list(&client).await.expect("fetch CM list");
    assert!(list.success, "CM list request failed: {}", list.message);

    let ws_server = list
        .serverlist
        .iter()
        .find(|server| server.server_type == CmServerType::Websockets)
        .expect("no websocket CM server in list");

    let mut conn = CmConnection::connect(&ws_server.endpoint)
        .await
        .expect("connect to CM");
    login::login_anonymous(&mut conn)
        .await
        .expect("anonymous login");
    conn
}

#[tokio::test]
#[ignore = "requires network access to Steam"]
async fn anonymous_login_and_product_info() {
    let mut conn = anonymous_connection().await;
    let session = conn.session().expect("session after login");
    assert!(session.steam_id != 0);

    let info = pics::get_product_info(&mut conn, &[test_app_id()], &[])
        .await
        .expect("product info");
    assert!(
        !info.apps.is_empty(),
        "app {} returned no product info",
        test_app_id()
    );

    conn.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires network access to Steam"]
async fn resolves_a_download_plan() {
    let mut conn = anonymous_connection().await;
    let config = DownloadConfig {
        app_id: test_app_id(),
        os: "linux".into(),
        branch: "public".into(),
    };

    let plan = depot::prepare_download(&mut conn, &config)
        .await
        .expect("prepare download plan");
    assert!(!plan.plans.is_empty(), "no depots resolved");
    assert!(plan.plans.iter().all(|plan| !plan.key.is_empty()));

    conn.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires STEAMDEPOT_USERNAME and STEAMDEPOT_REFRESH_TOKEN"]
async fn authenticated_token_login() {
    let (Ok(username), Ok(token)) = (
        std::env::var("STEAMDEPOT_USERNAME"),
        std::env::var("STEAMDEPOT_REFRESH_TOKEN"),
    ) else {
        eprintln!("skipping: STEAMDEPOT_USERNAME / STEAMDEPOT_REFRESH_TOKEN not set");
        return;
    };

    let client = reqwest::Client::new();
    let list = cm_list::get_cm_list(&client).await.expect("fetch CM list");
    let ws_server = list
        .serverlist
        .iter()
        .find(|server| server.server_type == CmServerType::Websockets)
        .expect("no websocket CM server in list");

    let mut conn = CmConnection::connect(&ws_server.endpoint)
        .await
        .expect("connect to CM");
    let session = login::login_with_token(&mut conn, &username, &token)
        .await
        .expect("token login");

    assert!(session.authenticated);
    assert!(session.steam_id != 0);

    conn.shutdown().await.unwrap();
}
