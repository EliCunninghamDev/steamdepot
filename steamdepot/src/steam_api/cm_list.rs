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
    let resp = client
        .get(CM_LIST_URL)
        .send()
        .await?
        .json::<CmListResponse>()
        .await?;
    Ok(resp.response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    {
      "response": {
        "success": true,
        "message": "",
        "serverlist": [
          {
            "endpoint": "cms1.example.com:443",
            "legacy_endpoint": "cms1.example.com:27017",
            "type": "websockets",
            "dc": "iad",
            "realm": "steamglobal",
            "load": 12,
            "wtd_load": 0.5
          },
          {
            "endpoint": "cms2.example.com:27017",
            "legacy_endpoint": "cms2.example.com:27017",
            "type": "netfilter",
            "dc": "sea",
            "realm": "steamglobal",
            "load": 7,
            "wtd_load": 0.25
          }
        ]
      }
    }
    "#;

    #[test]
    fn deserializes_server_list() {
        let parsed: CmListResponse = serde_json::from_str(SAMPLE).unwrap();
        assert!(parsed.response.success);
        assert_eq!(parsed.response.serverlist.len(), 2);
        assert_eq!(
            parsed.response.serverlist[0].server_type,
            CmServerType::Websockets
        );
        assert_eq!(
            parsed.response.serverlist[1].server_type,
            CmServerType::Netfilter
        );
        assert_eq!(parsed.response.serverlist[0].realm, CmRealm::SteamGlobal);

        let ws = parsed
            .response
            .serverlist
            .iter()
            .find(|s| s.server_type == CmServerType::Websockets)
            .unwrap();
        assert_eq!(ws.endpoint, "cms1.example.com:443");
    }

    #[test]
    fn serverlist_defaults_to_empty() {
        let parsed: CmListResponse =
            serde_json::from_str(r#"{"response":{"success":true}}"#).unwrap();
        assert!(parsed.response.serverlist.is_empty());
    }
}
