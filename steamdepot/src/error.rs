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

    #[error("steam error: {msg} (EResult {eresult}: {name})")]
    EResult { eresult: i32, name: &'static str, msg: String },

    #[error("no session set")]
    NoSession,

    #[error("service method timed out: {0}")]
    ServiceMethodTimeout(String),

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
            name: eresult_name(eresult),
            msg: msg.into(),
        }
    }
}

/// Map Steam EResult codes to human-readable names.
pub fn eresult_name(code: i32) -> &'static str {
    match code {
        1 => "OK",
        2 => "Fail",
        3 => "NoConnection",
        5 => "InvalidPassword",
        6 => "LoggedInElsewhere",
        7 => "InvalidProtocolVer",
        8 => "InvalidParam",
        9 => "FileNotFound",
        10 => "Busy",
        11 => "InvalidState",
        12 => "InvalidName",
        13 => "InvalidEmail",
        14 => "DuplicateName",
        15 => "AccessDenied",
        16 => "Timeout",
        17 => "Banned",
        18 => "AccountNotFound",
        20 => "Pending",
        21 => "EncryptionFailure",
        22 => "InsufficientPrivilege",
        24 => "Expired",
        25 => "AlreadyRedeemed",
        26 => "DuplicateRequest",
        27 => "AlreadyOwned",
        29 => "IPNotFound",
        30 => "PersistFailed",
        31 => "LockingFailed",
        32 => "LogonSessionReplaced",
        33 => "ConnectFailed",
        34 => "HandshakeFailed",
        35 => "IOFailure",
        36 => "RemoteDisconnect",
        42 => "PasswordUnset",
        50 => "RateLimitExceeded",
        63 => "AccountLoginDeniedNeedTwoFactor",
        65 => "AccountDisabled",
        84 => "TwoFactorCodeMismatch",
        85 => "TwoFactorActivationCodeMismatch",
        _ => "Unknown",
    }
}
