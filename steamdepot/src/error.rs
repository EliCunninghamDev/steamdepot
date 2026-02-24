use crate::emsg::EMsg;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("protobuf decode: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection closed")]
    ConnectionClosed,

    #[error("message too short ({0} bytes)")]
    MessageTooShort(usize),

    #[error("message truncated")]
    MessageTruncated,

    #[error("unknown EMsg: {0}")]
    UnknownEMsg(u32),

    #[error("unexpected EMsg: expected {expected:?}, got {got:?}")]
    UnexpectedEMsg { expected: EMsg, got: EMsg },

    #[error("steam error: {msg} (eresult={eresult})")]
    EResult { eresult: i32, msg: String },

    #[error("no session set")]
    NoSession,

    #[error("keyvalues parse error at byte {offset}: {msg}")]
    KvParse { offset: usize, msg: String },

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn eresult(eresult: i32, msg: impl Into<String>) -> Self {
        Self::EResult {
            eresult,
            msg: msg.into(),
        }
    }
}
