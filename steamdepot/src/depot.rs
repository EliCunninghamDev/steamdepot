use crate::cdn::{self, CdnAuthToken, CdnServer, DepotManifest};
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
    pub granted_appids: Vec<u32>,
    pub granted_packageids: Vec<u32>,
    /// CDN auth tokens obtained during manifest fetch, keyed by `"depot_id:host"`.
    pub cdn_auth_tokens: Vec<(u32, String, CdnAuthToken)>,
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

    // Request a free license for the app (and any depotfromapp references).
    // Skip app IDs we've already requested this session.
    let mut license_apps = vec![config.app_id];
    for d in &depots {
        if let Some(from) = d.depot_from_app {
            if !license_apps.contains(&from) {
                license_apps.push(from);
            }
        }
    }
    let is_authenticated = conn.session().map(|s| s.authenticated).unwrap_or(false);
    let cached = conn.session().map(|s| &s.licensed_appids).cloned().unwrap_or_default();
    let needed: Vec<u32> = license_apps.into_iter().filter(|id| !cached.contains(id)).collect();
    // Only attempt free license requests for authenticated sessions.
    // Anonymous sessions cannot claim licenses (Steam silently ignores the request).
    if !needed.is_empty() && is_authenticated {
        match pics::request_free_license(conn, &needed).await {
            Ok(resp) => {
                if resp.granted_appids.is_empty() && resp.granted_packageids.is_empty() {
                    eprintln!(
                        "info: free license request for apps {:?} returned no new grants \
                         (account may already own them)",
                        needed
                    );
                } else {
                    eprintln!(
                        "granted free licenses: apps={:?}, packages={:?}",
                        resp.granted_appids, resp.granted_packageids
                    );
                }
            }
            Err(e) => {
                return Err(Error::Other(format!(
                    "free license request failed for apps {:?}: {}. \
                     The account may not have access to this app. \
                     Ensure the free license has been claimed on the Steam account.",
                    needed, e
                )));
            }
        }
        // Cache all requested app IDs so we don't re-request
        if let Some(session) = conn.session_mut() {
            for &id in &needed {
                session.licensed_appids.insert(id);
            }
        }
    }

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
        granted_appids: Vec::new(),
        granted_packageids: Vec::new(),
        cdn_auth_tokens: Vec::new(),
    })
}

/// Fetch manifest request codes and CDN servers, then download manifests.
///
/// Call this after [`prepare_download`] to populate `manifest_request_code`,
/// `manifest`, and `cdn_servers` on the plan.
const MANIFEST_RETRIES: u32 = 3;

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

    if plan.cdn_servers.is_empty() {
        return Err(Error::Other("no CDN servers available".into()));
    }

    // Prefer SteamCache/CDN servers, but keep all for fallback rotation
    let preferred: Vec<_> = plan.cdn_servers.iter()
        .filter(|s| s.server_type == "SteamCache" || s.server_type == "CDN")
        .cloned()
        .collect();
    let servers = if preferred.is_empty() { &plan.cdn_servers } else { &preferred };

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

        let mut last_err = None;
        for attempt in 0..=MANIFEST_RETRIES {
            let server = &servers[attempt as usize % servers.len()];

            let result = cdn::download_manifest(
                client,
                server,
                dp.depot.depot_id,
                dp.depot.manifest_id,
                code,
                &dp.key,
                None,
            )
            .await;

            match result {
                Ok(m) => {
                    dp.manifest = Some(m);
                    last_err = None;
                    break;
                }
                Err(Error::Other(ref msg)) if msg.contains("403") => {
                    // CDN returned 403 — request an auth token and retry with same server
                    match cdn::request_cdn_auth_token(
                        conn,
                        dp.depot.depot_id,
                        &server.host,
                        plan.app_id,
                    )
                    .await
                    {
                        Ok(token) => {
                            match cdn::download_manifest(
                                client,
                                server,
                                dp.depot.depot_id,
                                dp.depot.manifest_id,
                                code,
                                &dp.key,
                                Some(&token.token),
                            )
                            .await
                            {
                                Ok(m) => {
                                    plan.cdn_auth_tokens.push((
                                        dp.depot.depot_id,
                                        server.host.clone(),
                                        token,
                                    ));
                                    dp.manifest = Some(m);
                                    last_err = None;
                                    break;
                                }
                                Err(e) => last_err = Some(e),
                            }
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                Err(e) => last_err = Some(e),
            }

            if attempt < MANIFEST_RETRIES {
                eprintln!(
                    "retrying manifest download for depot {} with next CDN server (attempt {}/{})",
                    dp.depot.depot_id, attempt + 2, MANIFEST_RETRIES + 1
                );
            }
        }

        if let Some(e) = last_err {
            return Err(e);
        }
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
