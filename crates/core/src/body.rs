//! Capped HTTP body reads (D111): a remote server can send a body of any size, with or without
//! `Content-Length`, so every download whose size matters reads in chunks and stops at a cap
//! instead of buffering the whole body first (`reqwest::Response::bytes`).

use std::fmt;

/// Why [`read_capped`] gave up.
#[derive(Debug)]
pub enum BodyError {
    /// The body (announced or received) is larger than the cap.
    TooLarge { cap: usize },
    /// The transfer failed (connection reset, timeout, bad chunk encoding).
    Http(reqwest::Error),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BodyError::TooLarge { cap } => write!(f, "body larger than {cap} bytes"),
            BodyError::Http(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BodyError {}

impl From<BodyError> for crate::Error {
    fn from(e: BodyError) -> Self {
        crate::Error::Network(e.to_string())
    }
}

/// Read the whole body of `resp`, at most `cap` bytes. A `Content-Length` above the cap fails
/// before anything is read; otherwise chunks are appended until the body ends or the next one
/// would pass the cap. Memory never exceeds `cap` plus one chunk.
pub async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, BodyError> {
    if resp
        .content_length()
        .is_some_and(|n| n > u64::try_from(cap).unwrap_or(u64::MAX))
    {
        return Err(BodyError::TooLarge { cap });
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(BodyError::Http)? {
        if out.len() + chunk.len() > cap {
            return Err(BodyError::TooLarge { cap });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
