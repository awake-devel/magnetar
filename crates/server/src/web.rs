//! The web UI served by the server (D132, docs/WEB.md): the files of `crates/server/web/`
//! embedded at build time, the SPA routes, the immutable asset path, the service worker and
//! manifest, and the security headers of every web response (D134).
//!
//! Mounted by `api::router` only when `AppState::web` is set (`MAGNETAR_WEB_UI`, default on).
//! Worker mode never mounts it.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::api::{ApiError, AppState};
use crate::config::Config;
use crate::web_auth::Sessions;

/// The embedded table written by `build.rs`: `ASSETS` (path, MIME, bytes; sorted by path) and
/// `VERSION` (16 hex digits of FNV-1a 64 over every file, or `"none"` without `web/`).
pub mod embedded {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// The Content-Security-Policy of every web response (D134). Trusted Types make any
/// HTML-parsing DOM sink throw in browsers that support them. The one allowed policy,
/// `magnetar` (`web/js/tt.js`), only has `createScriptURL` for `/sw.js` and the page's own
/// module workers: `trusted-types 'none'` would also block the service worker and the graph
/// worker, since their URLs are Trusted Types sinks too.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self' data: blob:; connect-src 'self'; font-src 'self'; worker-src 'self'; \
manifest-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
frame-ancestors 'none'; require-trusted-types-for 'script'; trusted-types magnetar";

pub const PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=(), payment=(), usb=()";

/// Cache rule of `/_app/{VERSION}/…`: the path changes with the content.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Paths of the single-page app that serve `index.html` (the client router takes over).
pub const SPA_ROUTES: &[&str] = &[
    "/",
    "/search",
    "/saved",
    "/stats",
    "/stats/graphs",
    "/stats/status",
    "/settings",
    "/login",
    "/crawl-map",
    "/graph",
];

const INDEX: &str = "index.html";
const SW: &str = "sw.js";
const MANIFEST: &str = "manifest.webmanifest";
/// What `sw.js` carries where the shell's URL list goes.
const ASSETS_MARKER: &str = "/*__ASSETS__*/[]";
const VERSION_MARKER: &str = "__V__";

/// The web UI's files, with the templated ones (`index.html`, `sw.js`, the manifest) rendered
/// once at startup.
pub struct WebUi {
    version: String,
    files: HashMap<String, (&'static str, Bytes)>,
    index: Option<Bytes>,
    sw: Option<Bytes>,
    manifest: Option<Bytes>,
}

impl WebUi {
    /// The files built into this binary.
    pub fn embedded() -> Self {
        Self::new(
            embedded::VERSION,
            embedded::ASSETS
                .iter()
                .map(|(p, m, b)| (p.to_string(), *m, Bytes::from_static(b))),
        )
    }

    /// A UI from any file set (tests use small synthetic ones).
    pub fn new(
        version: &str,
        files: impl IntoIterator<Item = (String, &'static str, Bytes)>,
    ) -> Self {
        let files: HashMap<_, _> = files.into_iter().map(|(p, m, b)| (p, (m, b))).collect();
        let render = |name: &str| {
            files
                .get(name)
                .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        };
        let mut shell: Vec<String> = files
            .keys()
            .filter(|p| !matches!(p.as_str(), INDEX | SW | MANIFEST))
            .map(|p| format!("/_app/{version}/{p}"))
            .collect();
        shell.sort();
        shell.insert(0, "/".into());
        let shell = serde_json::to_string(&shell).expect("string list");
        let index = render(INDEX).map(|s| Bytes::from(s.replace(VERSION_MARKER, version)));
        let sw = render(SW).map(|s| {
            Bytes::from(
                s.replace(ASSETS_MARKER, &shell)
                    .replace(VERSION_MARKER, version),
            )
        });
        let manifest = render(MANIFEST).map(|s| Bytes::from(s.replace(VERSION_MARKER, version)));
        Self {
            version: version.to_string(),
            files,
            index,
            sw,
            manifest,
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// No index page: a build without `web/` (or an incomplete one).
    pub fn is_empty(&self) -> bool {
        self.index.is_none()
    }

    /// Total bytes embedded.
    pub fn size(&self) -> usize {
        self.files.values().map(|(_, b)| b.len()).sum()
    }
}

/// Everything the web UI needs at run time (`AppState::web`).
pub struct WebState {
    pub ui: Arc<WebUi>,
    pub sessions: Sessions,
    /// `MAGNETAR_NTFY_TOPIC` from the environment; the `ntfy.topic` setting wins (D25).
    pub ntfy_env_topic: Option<String>,
}

impl WebState {
    pub fn new(ui: WebUi, ntfy_env_topic: Option<String>) -> Self {
        Self {
            ui: Arc::new(ui),
            sessions: Sessions::default(),
            ntfy_env_topic,
        }
    }

    /// The embedded UI with the process configuration; logs what is served.
    pub fn from_config(cfg: &Config) -> Self {
        let ui = WebUi::embedded();
        if ui.is_empty() {
            tracing::warn!(
                "web UI on (MAGNETAR_WEB_UI) but this build embeds no web/ files: \
                 the UI paths answer 404"
            );
        } else {
            tracing::info!(
                "web UI on: version {}, {} files, {} KiB",
                ui.version(),
                ui.files.len(),
                ui.size() / 1024
            );
        }
        Self::new(ui, cfg.ntfy_topic.clone())
    }

    /// Whether the server would send saved-search alerts (an ntfy topic in the settings or the
    /// environment).
    pub async fn ntfy(&self, store: &crate::store::PgStore) -> bool {
        crate::daemons::ntfy_topic(store, self.ntfy_env_topic.as_deref())
            .await
            .is_some()
    }
}

/// The web routes, merged by `api::router` when the UI is on.
pub fn routes() -> Router<AppState> {
    let mut r = Router::new();
    for path in SPA_ROUTES {
        r = r.route(path, get(index));
    }
    r.route("/torrent/{infohash}", get(torrent_page))
        .route("/title/{id}", get(title_page))
        .route("/_app/{version}/{*path}", get(asset))
        .route("/sw.js", get(service_worker))
        .route("/manifest.webmanifest", get(manifest))
}

/// The headers of every web response (D134).
pub fn set_web_headers(h: &mut HeaderMap) {
    let fixed: [(header::HeaderName, &'static str); 8] = [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            header::HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
        (
            header::HeaderName::from_static("permissions-policy"),
            PERMISSIONS_POLICY,
        ),
        (header::HeaderName::from_static("x-robots-tag"), "noindex"),
    ];
    for (k, v) in fixed {
        h.insert(k, HeaderValue::from_static(v));
    }
}

fn not_found() -> Response {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route").into_response()
}

fn ui(st: &AppState) -> Option<&WebUi> {
    let ui = &st.web.as_ref()?.ui;
    if ui.is_empty() {
        tracing::warn!("web UI requested but this build embeds no web/ files");
        return None;
    }
    Some(ui)
}

fn file(mime: &'static str, cache: &'static str, body: Bytes) -> Response {
    let mut r = Response::new(Body::from(body));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    set_web_headers(h);
    r
}

async fn index(State(st): State<AppState>) -> Response {
    match ui(&st).and_then(|u| u.index.clone()) {
        Some(b) => file("text/html; charset=utf-8", "no-cache", b),
        None => not_found(),
    }
}

/// Every unmatched path. A browser page load (`Sec-Fetch-Mode: navigate`, GET or HEAD) of an
/// unknown path outside the API gets the app with status 404, so the address bar shows the
/// app's "Not found" page instead of raw JSON. Everything else, and any request with the web
/// UI off, keeps the JSON `not_found` the API has always answered.
pub async fn fallback(State(st): State<AppState>, req: axum::extract::Request) -> Response {
    if page_load(&req)
        && let Some(b) = ui(&st).and_then(|u| u.index.clone())
    {
        let mut r = file("text/html; charset=utf-8", "no-cache", b);
        *r.status_mut() = StatusCode::NOT_FOUND;
        return r;
    }
    not_found()
}

fn page_load(req: &axum::extract::Request) -> bool {
    let path = req.uri().path();
    matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && req
        .headers()
        .get("sec-fetch-mode")
        .is_some_and(|v| v.as_bytes() == b"navigate")
        && !(path == "/api"
            || path.starts_with("/api/")
            || path.starts_with("/_app/")
            || path == "/relay"
            || path == "/health")
}

async fn torrent_page(State(st): State<AppState>, Path(ih): Path<String>) -> Response {
    if ih.len() == 40 && ih.bytes().all(|b| b.is_ascii_hexdigit()) {
        index(State(st)).await
    } else {
        not_found()
    }
}

async fn title_page(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    if id.parse::<i64>().is_ok() {
        index(State(st)).await
    } else {
        not_found()
    }
}

async fn asset(
    State(st): State<AppState>,
    Path((version, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(ui) = ui(&st) else {
        return not_found();
    };
    if version != ui.version {
        return not_found();
    }
    let Some((mime, body)) = ui.files.get(&path) else {
        return not_found();
    };
    let etag = format!("\"{}\"", ui.version);
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == etag || t == "*")
        });
    let mut r = if matches {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        let h = r.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
        set_web_headers(h);
        r
    } else {
        file(mime, IMMUTABLE, body.clone())
    };
    if let Ok(v) = HeaderValue::from_str(&etag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r
}

async fn service_worker(State(st): State<AppState>) -> Response {
    match ui(&st).and_then(|u| u.sw.clone()) {
        Some(b) => file("text/javascript; charset=utf-8", "no-cache", b),
        None => not_found(),
    }
}

async fn manifest(State(st): State<AppState>) -> Response {
    match ui(&st).and_then(|u| u.manifest.clone()) {
        Some(b) => file("application/manifest+json", "no-cache", b),
        None => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_ui_is_complete() {
        // A real build must not silently ship an empty UI (D132).
        let paths: Vec<&str> = embedded::ASSETS.iter().map(|(p, _, _)| *p).collect();
        for want in ["index.html", "sw.js", "manifest.webmanifest", "js/main.js"] {
            assert!(
                paths.contains(&want),
                "{want} is not embedded (VERSION {}); is crates/server/web/ there?",
                embedded::VERSION
            );
        }
        assert_ne!(embedded::VERSION, "none");
    }

    #[test]
    fn embedded_table_follows_the_build_rules() {
        let mut sorted = true;
        for w in embedded::ASSETS.windows(2) {
            sorted &= w[0].0 < w[1].0;
        }
        assert!(sorted, "ASSETS sorted by path");
        for (path, mime, bytes) in embedded::ASSETS {
            assert!(!crate::web_build::skipped(path), "{path} should be skipped");
            assert!(!path.starts_with("test/"), "{path}");
            assert_eq!(Some(*mime), crate::web_build::mime_for(path), "{path}");
            assert!(!bytes.is_empty() || path.ends_with(".gitkeep"), "{path}");
        }
        let v = crate::web_build::version(embedded::ASSETS.iter().map(|(p, _, b)| (*p, *b)));
        if !embedded::ASSETS.is_empty() {
            assert_eq!(v, embedded::VERSION);
        }
    }

    #[test]
    fn templates_are_rendered_once() {
        let ui = WebUi::new(
            "abc123",
            [
                (
                    "index.html".to_string(),
                    "text/html; charset=utf-8",
                    Bytes::from_static(b"<script src=\"/_app/__V__/js/main.js\"></script>"),
                ),
                (
                    "sw.js".to_string(),
                    "text/javascript; charset=utf-8",
                    Bytes::from_static(b"const V='__V__';const A=/*__ASSETS__*/[];"),
                ),
                (
                    "js/main.js".to_string(),
                    "text/javascript; charset=utf-8",
                    Bytes::from_static(b"x"),
                ),
                (
                    "css/app.css".to_string(),
                    "text/css; charset=utf-8",
                    Bytes::from_static(b"y"),
                ),
            ],
        );
        assert_eq!(
            ui.index.as_deref(),
            Some(&b"<script src=\"/_app/abc123/js/main.js\"></script>"[..])
        );
        assert_eq!(
            std::str::from_utf8(ui.sw.as_deref().unwrap()).unwrap(),
            "const V='abc123';const A=[\"/\",\"/_app/abc123/css/app.css\",\"/_app/abc123/js/main.js\"];"
        );
        assert!(ui.manifest.is_none());
        assert!(!ui.is_empty());
        assert!(WebUi::new("none", []).is_empty());
    }
}
