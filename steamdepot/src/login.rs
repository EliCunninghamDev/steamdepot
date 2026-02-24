use prost::Message;

use crate::connection::CmConnection;
use crate::emsg::EMsg;
use crate::error::{Error, Result};
use crate::proto::{CMsgClientLogon, CMsgClientLogonResponse, CMsgProtoBufHeader};
use crate::session::SessionState;

/// Anonymous SteamID: universe=Public(1), type=AnonUser(4), instance=0, account_id=0.
const ANONYMOUS_STEAM_ID: u64 = (1u64 << 56) | (4u64 << 52);

pub async fn login_anonymous(conn: &mut CmConnection) -> Result<SessionState> {
    let header = CMsgProtoBufHeader {
        steamid: Some(ANONYMOUS_STEAM_ID),
        ..Default::default()
    };

    let body = CMsgClientLogon {
        protocol_version: Some(65580),
        client_os_type: Some(20),
        ..Default::default()
    };

    conn.send(EMsg::ClientLogon, &header, &body.encode_to_vec())
        .await?;

    let msg = conn.recv().await?;

    if msg.emsg != EMsg::ClientLogOnResponse {
        return Err(Error::UnexpectedEMsg {
            expected: EMsg::ClientLogOnResponse,
            got: msg.emsg,
        });
    }

    let resp = CMsgClientLogonResponse::decode(msg.body.as_slice())?;

    let eresult = resp.eresult.unwrap_or(2);
    if eresult != 1 {
        return Err(Error::eresult(eresult, "logon failed"));
    }

    let session = SessionState {
        steam_id: msg.header.steamid.unwrap_or(0),
        session_id: msg.header.client_sessionid.unwrap_or(0),
        cell_id: resp.cell_id.unwrap_or(0),
        heartbeat_seconds: resp.heartbeat_seconds.unwrap_or(0),
    };

    conn.set_session(session);
    Ok(conn.session().unwrap().clone())
}
