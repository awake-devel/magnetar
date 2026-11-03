//! Web UI sign-in (D133, docs/API.md "Web sessions"): an HttpOnly, SameSite=Strict session
//! cookie issued for the API token, sessions in Postgres (`web_sessions`), CSRF tokens for
//! state-changing calls made with the cookie, and a server-wide limit on failed sign-ins.
//!
//! The bearer API is untouched: `api::auth` checks the bearer token first and falls back to the
//! cookie only on `/api/v1/` paths that are not worker, sync, downloads or `/relay` routes.
//! The token never reaches the browser's storage: it is sent once to `POST
//! /api/v1/web/login` and only a random session id comes back.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::{ApiError, AppState, ct_eq};
use crate::store::PgStore;

/// The session cookie's name (`__Host-`: Secure, Path=/, no Domain, so no other host can set
/// or shadow it). Always Secure, even on plain http to 127.0.0.1: browsers treat loopback as a
/// secure context and keep the cookie, and a proxy that rewrites `Host` (tailscale serve) can
/// never make the server hand out a non-Secure cookie for the https name (D133).
pub const COOKIE: &str = "__Host-magnetar_session";
/// Session lifetime (and the cookie's Max-Age).
pub const SESSION_TTL: i64 = 30 * 86400;
/// Largest sign-in body.
pub const LOGIN_BODY_LIMIT: usize = 1024;
/// Failed sign-ins allowed per [`FAIL_WINDOW`], server-wide; beyond, 429.
pub const MAX_FAILED_LOGINS: usize = 5;
pub const FAIL_WINDOW: Duration = Duration::from_secs(60);
/// How long a looked-up session is trusted from memory.
pub const CACHE_TTL: Duration = Duration::from_secs(60);
/// `last_seen` is written at most this often per session.
pub const LAST_SEEN_EVERY: i64 = 600;
/// Bytes of the user agent kept (display only).
const USER_AGENT_MAX: usize = 200;
/// Cached sessions kept before stale entries are pruned.
const CACHE_MAX: usize = 1024;

/// In-memory state of the web sessions: a short lookup cache and the failed sign-ins.
#[derive(Default)]
pub struct Sessions {
    cache: Mutex<HashMap<[u8; 32], Cached>>,
    failures: Mutex<VecDeque<Instant>>,
}

#[derive(Clone)]
struct Cached {
    session: WebSession,
    token_fp: String,
    fetched: Instant,
    last_seen: i64,
}

/// A live session.
#[derive(Clone, Debug)]
pub struct WebSession {
    pub id_hash: [u8; 32],
    pub csrf: String,
    pub created_at: i64,
    pub expires_at: i64,
}

impl Sessions {
    /// Wait before another sign-in may be tried, when the failure budget is spent.
    fn locked_for(&self, now: Instant) -> Option<Duration> {
        let mut f = self.failures.lock().unwrap();
        while f
            .front()
            .is_some_and(|t| now.duration_since(*t) >= FAIL_WINDOW)
        {
            f.pop_front();
        }
        (f.len() >= MAX_FAILED_LOGINS)
            .then(|| FAIL_WINDOW.saturating_sub(now.duration_since(*f.front().unwrap())))
    }

    fn failed(&self, now: Instant) {
        self.failures.lock().unwrap().push_back(now);
    }

    fn cached(&self, id: &[u8; 32], fp: &str, now: i64, at: Instant) -> Option<Cached> {
        let c = self.cache.lock().unwrap();
        let e = c.get(id)?;
        (at.duration_since(e.fetched) < CACHE_TTL && e.session.expires_at > now && e.token_fp == fp)
            .then(|| e.clone())
    }

    fn remember(&self, e: Cached) {
        let mut c = self.cache.lock().unwrap();
        if c.len() >= CACHE_MAX {
            let at = Instant::now();
            c.retain(|_, v| at.duration_since(v.fetched) < CACHE_TTL);
        }
        c.insert(e.session.id_hash, e);
    }

    fn forget(&self, id: &[u8; 32]) {
        self.cache.lock().unwrap().remove(id);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap().clear();
    }
}

/// First 16 hex digits of sha256(API token): sessions die when the token changes.
pub fn token_fp(token: &str) -> String {
    hex::encode(&Sha256::digest(token.as_bytes())[..8])
}

/// The row key of a cookie value: sha256 of its 32 random bytes. None for anything that is not
/// a 43-character base64url encoding of 32 bytes.
pub fn cookie_id_hash(value: &str) -> Option<[u8; 32]> {
    let raw = data_encoding::BASE64URL_NOPAD
        .decode(value.trim().as_bytes())
        .ok()?;
    (raw.len() == 32).then(|| Sha256::digest(&raw).into())
}

/// Values of the session cookie in the request (only the `__Host-` name counts).
pub fn session_cookies(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.trim().to_string())
        .collect()
}

/// The `Set-Cookie` value for a new session (`value` = Some) or a clearing one (None).
pub fn set_cookie(value: Option<&str>) -> String {
    let (value, age) = match value {
        Some(v) => (v, SESSION_TTL),
        None => ("", 0),
    };
    format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age={age}")
}

/// `Sec-Fetch-Site` absent (older browsers, non-browser clients), `same-origin` or `none`.
pub fn fetch_site_ok(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site") {
        None => true,
        Some(v) => matches!(v.to_str().map(str::trim), Ok("same-origin" | "none")),
    }
}

/// GET, HEAD and OPTIONS need no CSRF token.
pub fn safe_method(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// A state-changing call made with the cookie: same-origin and the session's CSRF token.
pub fn csrf_ok(headers: &HeaderMap, s: &WebSession) -> bool {
    fetch_site_ok(headers)
        && headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| ct_eq(t.trim().as_bytes(), s.csrf.as_bytes()))
}

/// Paths where the cookie stands in for the bearer token: the `/api/v1/` routes except the
/// worker endpoints, phone sync (`events/batch`) and downloads. Never `/relay` (no cross-site
/// WebSocket hijacking), never anything outside `/api/v1/`.
pub fn cookie_allowed(path: &str) -> bool {
    path.starts_with("/api/v1/")
        && !path.starts_with("/api/v1/worker/")
        && path != "/api/v1/events/batch"
        && path != "/api/v1/downloads"
        && !path.starts_with("/api/v1/downloads/")
}

pub fn forbidden_csrf() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "missing or bad CSRF token",
    )
}

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "no web session (sign in again)",
    )
}

fn db_error(e: sqlx::Error) -> ApiError {
    tracing::warn!("web sessions: {e}");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store",
        "database error (see the server log)",
    )
}

/// The session of the request's cookie, if any is live for the current token. Cached for
/// [`CACHE_TTL`]; `last_seen` is refreshed at most every [`LAST_SEEN_EVERY`] seconds.
pub async fn lookup(
    st: &AppState,
    web: &crate::web::WebState,
    headers: &HeaderMap,
) -> Result<Option<WebSession>, ApiError> {
    let now = magnetar_core::now();
    let at = Instant::now();
    let fp = token_fp(&st.token);
    for value in session_cookies(headers) {
        let Some(id) = cookie_id_hash(&value) else {
            continue;
        };
        let mut entry = match web.sessions.cached(&id, &fp, now, at) {
            Some(e) => e,
            None => {
                let row: Option<(String, String, i64, i64, i64)> = sqlx::query_as(
                    "SELECT csrf, token_fp, created_at, expires_at, last_seen FROM web_sessions
                     WHERE id_hash = $1 AND expires_at > $2",
                )
                .bind(&id[..])
                .bind(now)
                .fetch_optional(st.store.pool())
                .await
                .map_err(db_error)?;
                let Some((csrf, row_fp, created_at, expires_at, last_seen)) = row else {
                    web.sessions.forget(&id);
                    continue;
                };
                if !ct_eq(row_fp.as_bytes(), fp.as_bytes()) {
                    continue;
                }
                let e = Cached {
                    session: WebSession {
                        id_hash: id,
                        csrf,
                        created_at,
                        expires_at,
                    },
                    token_fp: row_fp,
                    fetched: at,
                    last_seen,
                };
                web.sessions.remember(e.clone());
                e
            }
        };
        if now - entry.last_seen >= LAST_SEEN_EVERY {
            sqlx::query("UPDATE web_sessions SET last_seen = $2 WHERE id_hash = $1")
                .bind(&id[..])
                .bind(now)
                .execute(st.store.pool())
                .await
                .map_err(db_error)?;
            entry.last_seen = now;
            web.sessions.remember(entry.clone());
        }
        return Ok(Some(entry.session));
    }
    Ok(None)
}

/// `/api/v1/web/*`, merged by `api::router` when the UI is on (outside the bearer middleware).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/web/login", post(login))
        .route("/api/v1/web/session", get(session))
        .route("/api/v1/web/logout", post(logout))
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

/// Answer of `login` and `session`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub csrf: String,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    pub ntfy: bool,
}

fn web_state(st: &AppState) -> Result<&crate::web::WebState, ApiError> {
    st.web
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route"))
}

fn json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    let ua = headers.get(header::USER_AGENT)?.to_str().ok()?;
    let mut end = ua.len().min(USER_AGENT_MAX);
    while !ua.is_char_boundary(end) {
        end -= 1;
    }
    Some(ua[..end].to_string())
}

/// `POST /api/v1/web/login` `{"token": …}`: a session cookie for the right API token.
async fn login(State(st): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    match login_inner(&st, &headers, body).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn login_inner(st: &AppState, headers: &HeaderMap, body: Body) -> Result<Response, ApiError> {
    let web = web_state(st)?;
    if !fetch_site_ok(headers) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "sign-in from another site",
        ));
    }
    if !json_content_type(headers) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_body",
            "expected Content-Type: application/json",
        ));
    }
    if let Some(wait) = web.sessions.locked_for(Instant::now()) {
        let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
        let mut r = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many failed sign-ins; try again later",
        )
        .into_response();
        r.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        return Ok(r);
    }
    let bytes = axum::body::to_bytes(body, LOGIN_BODY_LIMIT)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_body",
                "body too large",
            )
        })?;
    // The body is never echoed or logged: serde's message could quote it.
    let body: LoginBody = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "expected {\"token\": \"…\"}",
        )
    })?;
    if !ct_eq(body.token.trim().as_bytes(), st.token.as_bytes()) {
        web.sessions.failed(Instant::now());
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "token rejected",
        ));
    }
    let now = magnetar_core::now();
    let raw: [u8; 32] = rand::random();
    let cookie = data_encoding::BASE64URL_NOPAD.encode(&raw);
    let id: [u8; 32] = Sha256::digest(raw).into();
    let csrf = data_encoding::BASE64URL_NOPAD.encode(&rand::random::<[u8; 32]>());
    let expires_at = now + SESSION_TTL;
    let pool = st.store.pool();
    sqlx::query("DELETE FROM web_sessions WHERE expires_at <= $1")
        .bind(now)
        .execute(pool)
        .await
        .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO web_sessions (id_hash, csrf, token_fp, created_at, expires_at, last_seen,
                                   user_agent)
         VALUES ($1, $2, $3, $4, $5, $4, $6)",
    )
    .bind(&id[..])
    .bind(&csrf)
    .bind(token_fp(&st.token))
    .bind(now)
    .bind(expires_at)
    .bind(user_agent(headers))
    .execute(pool)
    .await
    .map_err(db_error)?;
    let info = SessionInfo {
        csrf,
        expires_at,
        created_at: Some(now),
        ntfy: web.ntfy(&st.store).await,
    };
    let mut r = Json(info).into_response();
    let set = HeaderValue::from_str(&set_cookie(Some(&cookie)))
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "cookie"))?;
    r.headers_mut().insert(header::SET_COOKIE, set);
    Ok(r)
}

/// `GET /api/v1/web/session`: the CSRF token and times of the cookie's session, or 401.
async fn session(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let web = web_state(&st)?;
    let s = lookup(&st, web, &headers).await?.ok_or_else(unauthorized)?;
    Ok(Json(SessionInfo {
        csrf: s.csrf,
        expires_at: s.expires_at,
        created_at: Some(s.created_at),
        ntfy: web.ntfy(&st.store).await,
    })
    .into_response())
}

#[derive(Deserialize)]
struct LogoutQuery {
    #[serde(default)]
    all: Option<String>,
}

/// `POST /api/v1/web/logout[?all=1]` (cookie + CSRF): ends this session, or every session.
async fn logout(
    State(st): State<AppState>,
    Query(q): Query<LogoutQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let web = web_state(&st)?;
    let s = lookup(&st, web, &headers).await?.ok_or_else(unauthorized)?;
    if !csrf_ok(&headers, &s) {
        return Err(forbidden_csrf());
    }
    let all = q
        .all
        .as_deref()
        .is_some_and(|v| matches!(v, "1" | "true" | "yes" | "on"));
    let pool = st.store.pool();
    if all {
        sqlx::query("DELETE FROM web_sessions")
            .execute(pool)
            .await
            .map_err(db_error)?;
        web.sessions.forget_all();
    } else {
        sqlx::query("DELETE FROM web_sessions WHERE id_hash = $1")
            .bind(&s.id_hash[..])
            .execute(pool)
            .await
            .map_err(db_error)?;
        web.sessions.forget(&s.id_hash);
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&set_cookie(None)) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(r)
}

/// Rows of `web_sessions` (tests, diagnostics).
pub async fn count(store: &PgStore) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM web_sessions")
        .fetch_one(store.pool())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(*k, HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn cookie_attributes() {
        assert_eq!(
            set_cookie(Some("v")),
            "__Host-magnetar_session=v; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=2592000"
        );
        assert_eq!(
            set_cookie(None),
            "__Host-magnetar_session=; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=0"
        );
    }

    #[test]
    fn cookie_values_and_paths() {
        let m = h(&[(
            "cookie",
            "a=1; magnetar_session=AAA; __Host-magnetar_session=BBB",
        )]);
        // A cookie without the `__Host-` prefix could have been set over plain http: ignored.
        assert_eq!(session_cookies(&m), ["BBB"]);
        let v = data_encoding::BASE64URL_NOPAD.encode(&[7u8; 32]);
        assert_eq!(v.len(), 43);
        assert!(cookie_id_hash(&v).is_some());
        assert!(cookie_id_hash("short").is_none());
        assert!(cookie_id_hash(&data_encoding::BASE64URL_NOPAD.encode(&[7u8; 31])).is_none());
        assert!(cookie_allowed("/api/v1/search"));
        assert!(cookie_allowed("/api/v1/torrents/x/probe/corrections"));
        for no in [
            "/relay",
            "/api/v1/events/batch",
            "/api/v1/downloads",
            "/api/v1/downloads/3/pause",
            "/api/v1/worker/heartbeat",
            "/health",
        ] {
            assert!(!cookie_allowed(no), "{no}");
        }
    }

    #[test]
    fn csrf_and_fetch_site() {
        let s = WebSession {
            id_hash: [0; 32],
            csrf: "tok".into(),
            created_at: 0,
            expires_at: 1,
        };
        assert!(csrf_ok(&h(&[("x-csrf-token", "tok")]), &s));
        assert!(csrf_ok(
            &h(&[("x-csrf-token", "tok"), ("sec-fetch-site", "same-origin")]),
            &s
        ));
        assert!(!csrf_ok(&h(&[("x-csrf-token", "tok2")]), &s));
        assert!(!csrf_ok(&h(&[]), &s));
        assert!(!csrf_ok(
            &h(&[("x-csrf-token", "tok"), ("sec-fetch-site", "same-site")]),
            &s
        ));
        assert!(!fetch_site_ok(&h(&[("sec-fetch-site", "cross-site")])));
        assert!(fetch_site_ok(&h(&[("sec-fetch-site", "none")])));
    }

    #[test]
    fn failure_budget() {
        let s = Sessions::default();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILED_LOGINS {
            assert!(s.locked_for(t0).is_none());
            s.failed(t0);
        }
        let wait = s.locked_for(t0 + Duration::from_secs(10)).unwrap();
        assert_eq!(wait, Duration::from_secs(50));
        assert!(s.locked_for(t0 + FAIL_WINDOW).is_none());
    }

    #[test]
    fn token_fingerprint() {
        assert_eq!(token_fp("a").len(), 16);
        assert_ne!(token_fp("a"), token_fp("b"));
    }
}
