use prost::Message;

use crate::connection::CmConnection;
use crate::emsg::EMsg;
use crate::error::{Error, Result};
use crate::proto::{
    c_msg_client_pics_product_info_request as req_types,
    CMsgClientGetDepotDecryptionKey, CMsgClientGetDepotDecryptionKeyResponse,
    CMsgClientPicsProductInfoRequest as PicsRequest,
    CMsgClientPicsProductInfoResponse as PicsResponse,
    CMsgClientRequestFreeLicense, CMsgClientRequestFreeLicenseResponse,
};

/// Returned product info for a single app.
#[derive(Debug, Clone)]
pub struct AppProductInfo {
    pub appid: u32,
    pub change_number: u32,
    pub buffer: Vec<u8>,
}

/// Returned product info for a single package.
#[derive(Debug, Clone)]
pub struct PackageProductInfo {
    pub packageid: u32,
    pub change_number: u32,
    pub buffer: Vec<u8>,
}

/// Result of a PICSGetProductInfo call.
#[derive(Debug, Default)]
pub struct ProductInfoResponse {
    pub apps: Vec<AppProductInfo>,
    pub unknown_appids: Vec<u32>,
    pub packages: Vec<PackageProductInfo>,
    pub unknown_packageids: Vec<u32>,
}

/// Fetch product info for the given app and/or package IDs.
///
/// Handles multi-part responses (`response_pending`) automatically, collecting
/// all chunks before returning.
pub async fn get_product_info(
    conn: &mut CmConnection,
    app_ids: &[u32],
    package_ids: &[u32],
) -> Result<ProductInfoResponse> {
    let header = conn.session_header()?;

    let body = PicsRequest {
        apps: app_ids
            .iter()
            .map(|&id| req_types::AppInfo {
                appid: Some(id),
                access_token: None,
                only_public_obsolete: None,
            })
            .collect(),
        packages: package_ids
            .iter()
            .map(|&id| req_types::PackageInfo {
                packageid: Some(id),
                access_token: None,
            })
            .collect(),
        meta_data_only: Some(false),
        single_response: Some(false),
        ..Default::default()
    };

    conn.send(
        EMsg::ClientPICSProductInfoRequest,
        &header,
        &body.encode_to_vec(),
    )
    .await?;

    let mut result = ProductInfoResponse::default();

    loop {
        let msg = conn.recv().await?;

        if msg.emsg != EMsg::ClientPICSProductInfoResponse {
            continue;
        }

        let resp = PicsResponse::decode(msg.body.as_slice())?;

        for app in resp.apps {
            result.apps.push(AppProductInfo {
                appid: app.appid.unwrap_or(0),
                change_number: app.change_number.unwrap_or(0),
                buffer: app.buffer.unwrap_or_default(),
            });
        }
        result.unknown_appids.extend(resp.unknown_appids);

        for pkg in resp.packages {
            result.packages.push(PackageProductInfo {
                packageid: pkg.packageid.unwrap_or(0),
                change_number: pkg.change_number.unwrap_or(0),
                buffer: pkg.buffer.unwrap_or_default(),
            });
        }
        result.unknown_packageids.extend(resp.unknown_packageids);

        if !resp.response_pending.unwrap_or(false) {
            break;
        }
    }

    Ok(result)
}

/// Fetch the decryption key for a depot.
///
/// Returns the raw key bytes on success, or an error if the server refuses
/// (e.g. the account doesn't own the app).
pub async fn get_depot_decryption_key(
    conn: &mut CmConnection,
    depot_id: u32,
    app_id: u32,
) -> Result<Vec<u8>> {
    let header = conn.session_header()?;

    let body = CMsgClientGetDepotDecryptionKey {
        depot_id: Some(depot_id),
        app_id: Some(app_id),
    };

    conn.send(
        EMsg::ClientGetDepotDecryptionKey,
        &header,
        &body.encode_to_vec(),
    )
    .await?;

    loop {
        let msg = conn.recv().await?;

        if msg.emsg != EMsg::ClientGetDepotDecryptionKeyResponse {
            continue;
        }

        let resp = CMsgClientGetDepotDecryptionKeyResponse::decode(msg.body.as_slice())?;

        let eresult = resp.eresult.unwrap_or(2);
        if eresult != 1 {
            let reason = match eresult {
                5 => "account doesn't own this depot",
                2 => "invalid depot/app combination",
                _ => "unknown error",
            };
            return Err(Error::eresult(
                eresult,
                format!("depot {} decryption key: {}", depot_id, reason),
            ));
        }

        return Ok(resp.depot_encryption_key.unwrap_or_default());
    }
}

/// Result of a free license request.
#[derive(Debug)]
pub struct FreeLicenseResponse {
    pub granted_appids: Vec<u32>,
    pub granted_packageids: Vec<u32>,
}

/// Request a free license for the given app IDs.
///
/// Steam grants free-to-play / free-to-download licenses on demand. This is
/// required for anonymous accounts to access dedicated server depots like
/// TF2 Classic (3557020).
pub async fn request_free_license(
    conn: &mut CmConnection,
    app_ids: &[u32],
) -> Result<FreeLicenseResponse> {
    let header = conn.session_header()?;

    let body = CMsgClientRequestFreeLicense {
        appids: app_ids.to_vec(),
    };

    conn.send(
        EMsg::ClientRequestFreeLicense,
        &header,
        &body.encode_to_vec(),
    )
    .await?;

    loop {
        let msg = conn.recv().await?;

        if msg.emsg != EMsg::ClientRequestFreeLicenseResponse {
            continue;
        }

        let resp = CMsgClientRequestFreeLicenseResponse::decode(msg.body.as_slice())?;

        let eresult = resp.eresult.unwrap_or(2);
        if eresult != 1 {
            return Err(Error::eresult(
                eresult as i32,
                format!("free license request for {:?}", app_ids),
            ));
        }

        return Ok(FreeLicenseResponse {
            granted_appids: resp.granted_appids,
            granted_packageids: resp.granted_packageids,
        });
    }
}
