//! The worker's HTTP client for the server's `/api/v1/worker/*` endpoints (D96): bearer worker
//! token, the [`API_HEADER`] on every request, and errors split into what stops the worker
//! (wrong token, incompatible server) and what is retried (network, 5xx).

use std::time::Duration;

use magnetar_core::api::ApiErrorBody;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::fleet::proto::*;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    /// The worker cannot go on: bad token, no worker API, incompatible version, disabled.
    #[error("{0}")]
    Fatal(String),
    /// Try again later: network error, timeout, server error, overload.
    #[error("{0}")]
    Retry(String),
}

impl ClientError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, ClientError::Fatal(_))
    }
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Client({})", self.base)
    }
}

/// Per request: large uploads may take a while on a slow uplink.
const TIMEOUT: Duration = Duration::from_secs(120);

impl Client {
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        let base = base.trim().trim_end_matches('/').to_string();
        anyhow::ensure!(
            base.starts_with("http://") || base.starts_with("https://"),
            "MAGNETAR_WORKER_SERVER must be an http(s):// URL"
        );
        Ok(Self {
            base,
            token: token.trim().to_string(),
            http: magnetar_core::http::client(TIMEOUT)?,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn post<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, ClientError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header(API_HEADER, WORKER_API.to_string())
            .json(body)
            .send()
            .await
            .map_err(|e| ClientError::Retry(format!("{path}: {}", without_url(&e))))?;
        let status = resp.status();
        if status.is_success() {
            if status == reqwest::StatusCode::NO_CONTENT {
                return serde_json::from_str("null")
                    .or_else(|_| serde_json::from_str("{}"))
                    .map_err(|e| ClientError::Retry(format!("{path}: {e}")));
            }
            return resp.json::<R>().await.map_err(|e| {
                ClientError::Retry(format!("{path}: bad answer: {}", without_url(&e)))
            });
        }
        let text = resp.text().await.unwrap_or_default();
        let body: Option<ApiErrorBody> = serde_json::from_str(&text).ok();
        let msg = body
            .as_ref()
            .map(|b| format!("{} ({})", b.message, b.error))
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(match status.as_u16() {
            401 => ClientError::Fatal(format!(
                "the server refused the worker token ({msg}): check MAGNETAR_WORKER_TOKEN against the server's MAGNETAR_WORKER_TOKENS"
            )),
            403 => ClientError::Fatal(format!("the server does not accept workers: {msg}")),
            404 if path == HELLO => ClientError::Fatal(format!(
                "{} has no worker API (404 on {HELLO}): it runs a Magnetar older than this worker ({}, worker API {WORKER_API}); upgrade the server",
                self.base,
                magnetar_core::VERSION
            )),
            409 => ClientError::Fatal(format!("incompatible server: {msg}")),
            _ => ClientError::Retry(format!("{path}: {msg}")),
        })
    }
}

/// reqwest errors print the URL; the base URL is fine to log, but keep lines short.
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s
            .replace(&format!(" ({u})"), "")
            .replace(u.as_str(), "<server>");
    }
    s
}
