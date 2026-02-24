use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;

use flate2::read::GzDecoder;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::emsg::EMsg;
use crate::error::{Error, Result};
use crate::proto::{CMsgClientHeartBeat, CMsgMulti, CMsgProtoBufHeader};
use crate::session::SessionState;

static NEXT_JOB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

type WsStreamInner = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStreamInner, tungstenite::Message>;
type WsStream = SplitStream<WsStreamInner>;

const PROTO_MASK: u32 = 0x80000000;

/// Parsed Steam CM message.
pub struct CmMessage {
    pub emsg: EMsg,
    pub header: CMsgProtoBufHeader,
    pub body: Vec<u8>,
}

/// Raw parsed frame, before EMsg lookup.
struct RawFrame {
    emsg_val: u32,
    header: CMsgProtoBufHeader,
    body: Vec<u8>,
}

/// Shared write half of the WebSocket, wrapped for concurrent access.
pub type SharedSink = Arc<Mutex<WsSink>>;

/// Connection to a Steam CM server.
///
/// Owns the WebSocket sink (shared via `Arc<Mutex>`) and the read stream.
/// After login, call [`start_heartbeat`] to spawn a background task.
///
/// Use [`shutdown`] for a clean disconnect (sends `ClientLogOff`, closes the
/// WebSocket). If dropped without calling `shutdown`, the heartbeat task is
/// aborted but no logoff is sent.
pub struct CmConnection {
    pub sink: SharedSink,
    stream: CmStream,
    session: Option<SessionState>,
    heartbeat_handle: Option<JoinHandle<()>>,
}

impl CmConnection {
    /// Connect to a CM server's WebSocket endpoint.
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let url = format!("wss://{}/cmsocket/", endpoint);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
        let (sink, stream) = ws.split();
        Ok(Self {
            sink: Arc::new(Mutex::new(sink)),
            stream: CmStream::new(stream),
            session: None,
            heartbeat_handle: None,
        })
    }

    /// Send a protobuf-framed message.
    pub async fn send(
        &self,
        emsg: EMsg,
        header: &CMsgProtoBufHeader,
        body: &[u8],
    ) -> Result<()> {
        send_raw(&mut *self.sink.lock().await, emsg, header, body).await
    }

    /// Receive the next message, unwrapping Multi envelopes and skipping unknown EMsgs.
    pub async fn recv(&mut self) -> Result<CmMessage> {
        self.stream.recv().await
    }

    /// Store session state. Called after a successful login.
    pub fn set_session(&mut self, session: SessionState) {
        self.session = Some(session);
    }

    /// Start the heartbeat background task.
    /// Must be called after [`set_session`].
    pub fn start_heartbeat(&mut self) {
        let session = match &self.session {
            Some(s) => s,
            None => return,
        };

        let seconds = session.heartbeat_seconds;
        if seconds <= 0 {
            return;
        }

        let sink = Arc::clone(&self.sink);
        let header = CMsgProtoBufHeader {
            steamid: Some(session.steam_id),
            client_sessionid: Some(session.session_id),
            ..Default::default()
        };

        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(seconds as u64));
            interval.tick().await; // first tick fires immediately — skip it

            loop {
                interval.tick().await;
                let body = CMsgClientHeartBeat::default().encode_to_vec();
                let mut sink = sink.lock().await;
                if send_raw(&mut sink, EMsg::ClientHeartBeat, &header, &body)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        self.heartbeat_handle = Some(handle);
    }

    /// Cleanly shut down the connection: stop heartbeat, send `ClientLogOff`,
    /// and close the WebSocket.
    pub async fn shutdown(&mut self) -> Result<()> {
        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
            let _ = handle.await;
        }

        if let Some(session) = &self.session {
            let header = CMsgProtoBufHeader {
                steamid: Some(session.steam_id),
                client_sessionid: Some(session.session_id),
                ..Default::default()
            };
            let _ = self.send(EMsg::ClientLogOff, &header, &[]).await;
        }

        let mut sink = self.sink.lock().await;
        let _ = sink.send(tungstenite::Message::Close(None)).await;
        let _ = sink.close().await;

        Ok(())
    }

    /// Access the stored session state, if set.
    pub fn session(&self) -> Option<&SessionState> {
        self.session.as_ref()
    }

    /// Call a Steam service method (non-authed) and wait for the response.
    ///
    /// `method` is the fully qualified method name, e.g.
    /// `"ContentServerDirectory.GetManifestRequestCode#1"`.
    ///
    /// Returns the raw response body bytes.
    pub async fn service_method_call(
        &mut self,
        method: &str,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let job_id = NEXT_JOB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let mut header = self.session_header()?;
        header.target_job_name = Some(method.to_string());
        header.jobid_source = Some(job_id);

        self.send(
            EMsg::ServiceMethodCallFromClientNonAuthed,
            &header,
            body,
        )
        .await?;

        loop {
            let msg = self.recv().await?;
            if msg.emsg != EMsg::ServiceMethodResponse
                && msg.emsg != EMsg::ServiceMethodSendToClient
            {
                continue;
            }
            if msg.header.jobid_target == Some(job_id) {
                let eresult = msg.header.eresult.unwrap_or(2);
                if eresult != 1 {
                    return Err(Error::eresult(
                        eresult,
                        format!("service method {}", method),
                    ));
                }
                return Ok(msg.body);
            }
        }
    }

    /// Build a `CMsgProtoBufHeader` populated with the current session's
    /// steam_id and client_sessionid.
    pub fn session_header(&self) -> Result<CMsgProtoBufHeader> {
        let s = self.session.as_ref().ok_or(Error::NoSession)?;
        Ok(CMsgProtoBufHeader {
            steamid: Some(s.steam_id),
            client_sessionid: Some(s.session_id),
            ..Default::default()
        })
    }
}

impl Drop for CmConnection {
    fn drop(&mut self) {
        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Internal framing helpers
// ---------------------------------------------------------------------------

async fn send_raw(
    sink: &mut WsSink,
    emsg: EMsg,
    header: &CMsgProtoBufHeader,
    body: &[u8],
) -> Result<()> {
    let header_bytes = header.encode_to_vec();

    let mut buf = Vec::with_capacity(4 + 4 + header_bytes.len() + body.len());
    buf.extend_from_slice(&(emsg as u32 | PROTO_MASK).to_le_bytes());
    buf.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&header_bytes);
    buf.extend_from_slice(body);

    sink.send(tungstenite::Message::Binary(buf.into())).await?;
    Ok(())
}

fn parse_raw_frame(data: &[u8]) -> Result<RawFrame> {
    if data.len() < 8 {
        return Err(Error::MessageTooShort(data.len()));
    }

    let raw_emsg = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let emsg_val = raw_emsg & !PROTO_MASK;

    let header_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if data.len() < 8 + header_len {
        return Err(Error::MessageTruncated);
    }

    let header = CMsgProtoBufHeader::decode(&data[8..8 + header_len])?;
    let body = data[8 + header_len..].to_vec();

    Ok(RawFrame {
        emsg_val,
        header,
        body,
    })
}

// ---------------------------------------------------------------------------
// CmStream — buffered reader with CMsgMulti unwrapping
// ---------------------------------------------------------------------------

struct CmStream {
    stream: WsStream,
    queue: VecDeque<Vec<u8>>,
}

impl CmStream {
    fn new(stream: WsStream) -> Self {
        Self {
            stream,
            queue: VecDeque::new(),
        }
    }

    async fn recv(&mut self) -> Result<CmMessage> {
        loop {
            let raw = match self.queue.pop_front() {
                Some(data) => parse_raw_frame(&data)?,
                None => parse_raw_frame(&self.recv_raw().await?)?,
            };

            if raw.emsg_val == EMsg::Multi as u32 {
                self.unpack_multi(&raw.body)?;
                continue;
            }

            match EMsg::from_u32(raw.emsg_val) {
                Some(emsg) => {
                    return Ok(CmMessage {
                        emsg,
                        header: raw.header,
                        body: raw.body,
                    })
                }
                None => continue,
            }
        }
    }

    fn unpack_multi(&mut self, body: &[u8]) -> Result<()> {
        let multi = CMsgMulti::decode(body)?;
        let payload = match (multi.message_body, multi.size_unzipped) {
            (Some(body), Some(size)) if size > 0 => {
                let mut decompressed = Vec::with_capacity(size as usize);
                GzDecoder::new(body.as_slice()).read_to_end(&mut decompressed)?;
                decompressed
            }
            (Some(body), _) => body,
            (None, _) => return Ok(()),
        };

        let mut offset = 0;
        while offset + 4 <= payload.len() {
            let len =
                u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if offset + len > payload.len() {
                break;
            }
            self.queue.push_back(payload[offset..offset + len].to_vec());
            offset += len;
        }
        Ok(())
    }

    async fn recv_raw(&mut self) -> Result<Vec<u8>> {
        loop {
            let msg = self
                .stream
                .next()
                .await
                .ok_or(Error::ConnectionClosed)??;
            match msg {
                tungstenite::Message::Binary(data) => return Ok(data.into()),
                tungstenite::Message::Close(_) => return Err(Error::ConnectionClosed),
                _ => continue,
            }
        }
    }
}
