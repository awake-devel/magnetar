//! Crate-wide error type.

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("event rejected: {0:?}")]
    Rejected(crate::model::Rejection),
    #[error("network: {0}")]
    Network(String),
    #[error("relay {relay}: {msg}")]
    Relay { relay: String, msg: String },
    #[error("server returned {status}: {msg}")]
    Server { status: u16, msg: String },
    #[error("store: {0}")]
    Store(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    Other(String),
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(format!("json: {e}"))
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e.to_string())
    }
}
