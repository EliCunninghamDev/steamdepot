use serde::{Deserialize, Serialize};

const CM_LIST_URL: &str =
    "https://api.steampowered.com/ISteamDirectory/GetCMListForConnect/v0001/?cellid=0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CmServerType {
    Netfilter,
    Websockets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CmRealm {
    #[serde(rename = "steamglobal")]
    SteamGlobal,
    #[serde(rename = "steamchina")]
    SteamChina,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CmListResponse {
    pub response: CmListResponseInner,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CmListResponseInner {
    pub success: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub serverlist: Vec<CmServer>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CmServer {
    pub endpoint: String,
    pub legacy_endpoint: String,
    #[serde(rename = "type")]
    pub server_type: CmServerType,
    pub dc: String,
    pub realm: CmRealm,
    pub load: u32,
    pub wtd_load: f64,
}

pub async fn get_cm_list(client: &reqwest::Client) -> Result<CmListResponseInner, reqwest::Error> {
    let resp = client.get(CM_LIST_URL).send().await?.json::<CmListResponse>().await?;
    Ok(resp.response)
}
