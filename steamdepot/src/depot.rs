use crate::cdn::{self, CdnServer, DepotManifest};
use crate::connection::CmConnection;
use crate::error::{Error, Result};
use crate::keyvalues::{self, KvValue};
use crate::pics;

/// Configuration for a download operation.
pub struct DownloadConfig {
    pub app_id: u32,
    pub os: String,
    pub branch: String,
}

/// A resolved depot ready for download.
#[derive(Debug, Clone)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub manifest_id: u64,
    /// If set, use this app ID when requesting depot keys / manifest codes.
    pub depot_from_app: Option<u32>,
}

impl DepotInfo {
    /// The app ID to use for depot key and manifest code requests.
    /// Falls back to the parent app ID if `depot_from_app` is not set.
    pub fn containing_app(&self, parent_app_id: u32) -> u32 {
        self.depot_from_app.unwrap_or(parent_app_id)
    }
}

/// A depot with its decryption key resolved.
#[derive(Debug)]
pub struct DepotPlan {
    pub depot: DepotInfo,
    pub key: Vec<u8>,
    pub manifest_request_code: Option<u64>,
    pub manifest: Option<DepotManifest>,
}

/// The full plan for downloading an app's content.
#[derive(Debug)]
pub struct DownloadPlan {
    pub app_id: u32,
    pub plans: Vec<DepotPlan>,
    pub cdn_servers: Vec<CdnServer>,
}

/// Prepare a download plan: fetch app info, resolve depots, obtain keys.
pub async fn prepare_download(
    conn: &mut CmConnection,
    config: &DownloadConfig,
) -> Result<DownloadPlan> {
    let app_info = pics::get_product_info(conn, &[config.app_id], &[])
        .await?
        .apps
        .into_iter()
        .next()
        .ok_or_else(|| Error::Other(format!("app {} not found", config.app_id)))?;

    let depots = resolve_depots(&app_info, &config.os, &config.branch)?;

    let mut plans = Vec::new();
    for depot in depots {
        let key_app = depot.containing_app(config.app_id);
        let key = pics::get_depot_decryption_key(conn, depot.depot_id, key_app).await?;
        plans.push(DepotPlan {
            depot,
            key,
            manifest_request_code: None,
            manifest: None,
        });
    }

    Ok(DownloadPlan {
        app_id: config.app_id,
        plans,
        cdn_servers: Vec::new(),
    })
}

/// Fetch manifest request codes and CDN servers, then download manifests.
///
/// Call this after [`prepare_download`] to populate `manifest_request_code`,
/// `manifest`, and `cdn_servers` on the plan.
pub async fn fetch_manifests(
    conn: &mut CmConnection,
    client: &reqwest::Client,
    plan: &mut DownloadPlan,
    branch: &str,
) -> Result<()> {
    let cell_id = conn
        .session()
        .map(|s| s.cell_id)
        .unwrap_or(0);

    plan.cdn_servers = cdn::get_cdn_servers(conn, cell_id).await?;

    let cdn_server = plan
        .cdn_servers
        .iter()
        .find(|s| s.server_type == "SteamCache" || s.server_type == "CDN")
        .or_else(|| plan.cdn_servers.first())
        .ok_or_else(|| Error::Other("no CDN servers available".into()))?
        .clone();

    for dp in &mut plan.plans {
        let key_app = dp.depot.containing_app(plan.app_id);

        let code = cdn::get_manifest_request_code(
            conn,
            key_app,
            dp.depot.depot_id,
            dp.depot.manifest_id,
            branch,
        )
        .await?;
        dp.manifest_request_code = Some(code);

        let manifest = cdn::download_manifest(
            client,
            &cdn_server,
            dp.depot.depot_id,
            dp.depot.manifest_id,
            code,
            &dp.key,
        )
        .await?;
        dp.manifest = Some(manifest);
    }

    Ok(())
}

/// Resolve downloadable depots from a PICSGetProductInfo app response.
///
/// Parses the KeyValues buffer, filters depots by `os`, and resolves the
/// manifest ID for `branch` (falls back to `"public"`).
pub fn resolve_depots(
    app: &pics::AppProductInfo,
    os: &str,
    branch: &str,
) -> Result<Vec<DepotInfo>> {
    let root = keyvalues::parse(&app.buffer)?;

    let appinfo = root
        .get("appinfo")
        .ok_or_else(|| Error::Other("missing \"appinfo\" key".into()))?;

    let depots = match appinfo.get_children("depots") {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };

    let mut result = Vec::new();

    for (key, value) in depots {
        let depot_id: u32 = match key.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        let depot = match value.as_children() {
            Some(c) => c,
            None => continue,
        };

        // Filter by OS — no oslist means all platforms.
        if let Some(KvValue::Children(config)) = depot.get("config") {
            if let Some(KvValue::String(oslist)) = config.get("oslist") {
                if !oslist.split(',').any(|o| o.trim() == os) {
                    continue;
                }
            }
        }

        let manifests = match depot.get("manifests") {
            Some(KvValue::Children(m)) => m,
            _ => continue,
        };

        // Manifest entry can be either:
        //   "public" "1234567890"              (plain string)
        //   "public" { "gid" "1234567890" }    (child object with gid)
        let manifest_val = manifests.get(branch).or_else(|| {
            if branch != "public" {
                manifests.get("public")
            } else {
                None
            }
        });

        let manifest_str = match manifest_val {
            Some(KvValue::String(s)) => s.as_str(),
            Some(KvValue::Children(map)) => match map.get("gid") {
                Some(KvValue::String(s)) => s.as_str(),
                _ => continue,
            },
            None => continue,
        };

        let manifest_id: u64 = match manifest_str.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        let depot_from_app = depot
            .get("depotfromapp")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());

        result.push(DepotInfo {
            depot_id,
            manifest_id,
            depot_from_app,
        });
    }

    Ok(result)
}
