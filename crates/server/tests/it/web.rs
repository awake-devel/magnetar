//! Web UI serving (D132), web sessions (D133) and the security headers (D134) against an
//! in-process server on 127.0.0.1 (needs MAGNETAR_TEST_DATABASE_URL, else SKIPPED). The UI is a
//! small synthetic file set with the same templates as the real one, so these tests do not
//! depend on the client's content.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use magnetar_core::store::Store;
use magnetar_server::web::{CSP, WebState, WebUi};
use magnetar_server::{AppState, PgStore};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::common::*;

const V: &str = "0123456789abcdef";

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn test_ui() -> WebUi {
    let f = |p: &str, m: &'static str, b: &'static [u8]| (p.to_string(), m, Bytes::from_static(b));
    WebUi::new(
        V,
        [
            f(
                "index.html",
                "text/html; charset=utf-8",
                b"<!doctype html><link rel=stylesheet href=\"/_app/__V__/css/app.css\">\
<script type=module src=\"/_app/__V__/js/main.js\"></script><div id=app></div>",
            ),
            f(
                "sw.js",
                "text/javascript; charset=utf-8",
                b"const VERSION = \"__V__\";\nconst ASSETS = /*__ASSETS__*/[];\n",
            ),
            f(
                "manifest.webmanifest",
                "application/manifest+json",
                b"{\"name\":\"Magnetar\",\"icons\":[{\"src\":\"/_app/__V__/icons/icon-192.png\"}]}",
            ),
            f(
                "js/main.js",
                "text/javascript; charset=utf-8",
                b"export {};\n",
            ),
            f("css/app.css", "text/css; charset=utf-8", b"body{}\n"),
            f("icons/icon-192.png", "image/png", b"\x89PNG\r\n"),
        ],
    )
}

/// A server with the web UI on (`ntfy_topic`: the env topic it was started with).
async fn spawn_web(store: &PgStore, token: &str, ntfy_topic: Option<&str>) -> String {
    let web = WebState::new(test_ui(), ntfy_topic.map(str::to_string));
    spawn_state(AppState::new(Arc::new(store.clone()), token).with_web(Some(Arc::new(web)))).await
}

async fn spawn_state(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

fn header<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

async fn json_error(r: reqwest::Response) -> (StatusCode, String) {
    let status = r.status();
    let v: Value = r.json().await.unwrap();
    (status, v["error"].as_str().unwrap_or_default().to_string())
}

/// What every web response carries (D134).
fn assert_web_headers(h: &HeaderMap, what: &str) {
    assert_eq!(header(h, "content-security-policy"), CSP, "{what}");
    assert_eq!(header(h, "x-content-type-options"), "nosniff", "{what}");
    assert_eq!(header(h, "referrer-policy"), "no-referrer", "{what}");
    assert_eq!(header(h, "x-frame-options"), "DENY", "{what}");
    assert_eq!(
        header(h, "cross-origin-opener-policy"),
        "same-origin",
        "{what}"
    );
    assert_eq!(
        header(h, "cross-origin-resource-policy"),
        "same-origin",
        "{what}"
    );
    assert_eq!(
        header(h, "permissions-policy"),
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        "{what}"
    );
    assert_eq!(header(h, "x-robots-tag"), "noindex", "{what}");
    assert!(!h.contains_key("access-control-allow-origin"), "{what}");
}

/// What every API answer carries (D134).
fn assert_api_headers(h: &HeaderMap, what: &str) {
    assert_eq!(header(h, "x-content-type-options"), "nosniff", "{what}");
    assert_eq!(header(h, "referrer-policy"), "no-referrer", "{what}");
    assert!(!h.contains_key("content-security-policy"), "{what}");
}

/// A signed-in browser: its cookie header and CSRF token.
struct Browser {
    cookie: String,
    csrf: String,
}

async fn login(base: &str, token: &str) -> Browser {
    let r = http()
        .post(format!("{base}/api/v1/web/login"))
        .json(&json!({"token": token}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let set = header(r.headers(), "set-cookie").to_string();
    let pair = set.split(';').next().unwrap().to_string();
    let v: Value = r.json().await.unwrap();
    Browser {
        cookie: pair,
        csrf: v["csrf"].as_str().unwrap().to_string(),
    }
}

impl Browser {
    fn req(&self, m: Method, url: String) -> reqwest::RequestBuilder {
        http().request(m, url).header("cookie", &self.cookie)
    }
}

#[tokio::test]
async fn web_off_serves_nothing_and_ignores_cookies() {
    let Some(db) = test_db().await else { return };
    let off = spawn_server(&db.store).await;
    let on = spawn_web(&db.store, TOKEN, None).await;
    let c = http();
    for path in ["/", "/saved", "/sw.js", "/manifest.webmanifest"] {
        let r = c.get(format!("{off}{path}")).send().await.unwrap();
        assert_api_headers(r.headers(), path);
        assert_eq!(
            json_error(r).await,
            (StatusCode::NOT_FOUND, "not_found".into()),
            "{path}"
        );
    }
    let r = c
        .post(format!("{off}/api/v1/web/login"))
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    // A valid session (made on the web server, same database) means nothing without the UI.
    let b = login(&on, TOKEN).await;
    let r = b
        .req(Method::GET, format!("{on}/api/v1/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = b
        .req(Method::GET, format!("{off}/api/v1/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_error(r).await,
        (StatusCode::UNAUTHORIZED, "unauthorized".into())
    );
}

#[tokio::test]
async fn web_pages_assets_and_headers() {
    let Some(db) = test_db().await else { return };
    let base = spawn_web(&db.store, TOKEN, None).await;
    let off = spawn_server(&db.store).await;
    let c = http();

    let r = c.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_web_headers(r.headers(), "/");
    assert_eq!(
        header(r.headers(), "content-type"),
        "text/html; charset=utf-8"
    );
    assert_eq!(header(r.headers(), "cache-control"), "no-cache");
    let body = r.text().await.unwrap();
    assert!(body.contains(&format!("/_app/{V}/js/main.js")), "{body}");
    assert!(!body.contains("__V__"));

    let ih = "0123456789abcdef0123456789abcdef01234567";
    for path in [
        "/search",
        "/saved",
        "/stats",
        "/stats/graphs",
        "/stats/status",
        "/settings",
        "/login",
        "/crawl-map",
        "/graph?path=wot",
        &format!("/torrent/{ih}"),
        "/title/7",
        "/title/-3",
    ] {
        let r = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert_web_headers(r.headers(), path);
        assert_eq!(r.text().await.unwrap(), body, "{path}");
    }
    for path in [
        "/torrent/xyz",
        &format!("/torrent/{ih}0"),
        "/title/abc",
        "/api/v1/nope",
        "/nope",
        "/_app",
        "/settings/x",
    ] {
        let r = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_api_headers(r.headers(), path);
        assert_eq!(
            json_error(r).await,
            (StatusCode::NOT_FOUND, "not_found".into()),
            "{path}"
        );
    }
    // A browser page load of an unknown path gets the app (its "Not found" view) with 404;
    // API, asset, /relay and /health paths keep the JSON answer, and so does the UI-off server.
    for path in ["/nope", "/settings/x", "/stats/nope/x"] {
        let r = c
            .get(format!("{base}{path}"))
            .header("sec-fetch-mode", "navigate")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{path}");
        assert_web_headers(r.headers(), path);
        assert_eq!(r.text().await.unwrap(), body, "{path}");
    }
    for (b, path) in [
        (&base, "/api/v1/nope"),
        (&base, "/api"),
        (&base, "/_app/x/y.js"),
        (&base, "/relay"),
        (&off, "/nope"),
    ] {
        let r = c
            .get(format!("{b}{path}"))
            .header("sec-fetch-mode", "navigate")
            .send()
            .await
            .unwrap();
        assert_api_headers(r.headers(), path);
        let (code, err) = json_error(r).await;
        assert!(
            code == StatusCode::NOT_FOUND || code == StatusCode::UNAUTHORIZED,
            "{path}: {code}"
        );
        assert!(err == "not_found" || err == "unauthorized", "{path}: {err}");
    }
    // HEAD is answered.
    let r = c.head(format!("{base}/saved")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_web_headers(r.headers(), "HEAD /saved");

    // Versioned assets: immutable, ETag, 304; another version is gone.
    let main = format!("{base}/_app/{V}/js/main.js");
    let r = c.get(&main).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_web_headers(r.headers(), "main.js");
    assert_eq!(
        header(r.headers(), "content-type"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        header(r.headers(), "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(header(r.headers(), "etag"), format!("\"{V}\""));
    assert_eq!(r.text().await.unwrap(), "export {};\n");
    let r = c
        .get(&main)
        .header("if-none-match", format!("\"{V}\""))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header(r.headers(), "cache-control"),
        "public, max-age=31536000, immutable"
    );
    let r = c
        .get(format!("{base}/_app/{V}/icons/icon-192.png"))
        .send()
        .await
        .unwrap();
    assert_eq!(header(r.headers(), "content-type"), "image/png");
    for path in [
        "/_app/ffffffffffffffff/js/main.js".to_string(),
        format!("/_app/{V}/js/nope.js"),
        format!("/_app/{V}/../Cargo.toml"),
    ] {
        let r = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{path}");
    }

    // The service worker: version and shell list filled in, never cached by HTTP.
    let r = c.get(format!("{base}/sw.js")).send().await.unwrap();
    assert_web_headers(r.headers(), "sw.js");
    assert_eq!(
        header(r.headers(), "content-type"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(header(r.headers(), "cache-control"), "no-cache");
    let sw = r.text().await.unwrap();
    assert!(sw.contains(&format!("const VERSION = \"{V}\"")), "{sw}");
    let list = sw
        .split("const ASSETS = ")
        .nth(1)
        .unwrap()
        .trim_end()
        .trim_end_matches(';');
    let list: Vec<String> = serde_json::from_str(list).unwrap();
    assert_eq!(
        list,
        [
            "/".to_string(),
            format!("/_app/{V}/css/app.css"),
            format!("/_app/{V}/icons/icon-192.png"),
            format!("/_app/{V}/js/main.js"),
        ]
    );

    let r = c
        .get(format!("{base}/manifest.webmanifest"))
        .send()
        .await
        .unwrap();
    assert_web_headers(r.headers(), "manifest");
    assert_eq!(
        header(r.headers(), "content-type"),
        "application/manifest+json"
    );
    assert_eq!(header(r.headers(), "cache-control"), "no-cache");
    let m: Value = r.json().await.unwrap();
    assert_eq!(
        m["icons"][0]["src"],
        format!("/_app/{V}/icons/icon-192.png")
    );

    // /health: the same body with the UI on or off, plus the API headers.
    let h_on = c.get(format!("{base}/health")).send().await.unwrap();
    let h_off = c.get(format!("{off}/health")).send().await.unwrap();
    for (r, what) in [(&h_on, "health on"), (&h_off, "health off")] {
        assert_api_headers(r.headers(), what);
        assert_eq!(header(r.headers(), "cache-control"), "no-store", "{what}");
    }
    assert_eq!(h_on.bytes().await.unwrap(), h_off.bytes().await.unwrap());

    // API answers: nosniff, no-referrer, no-store, with the UI on or off.
    for b in [&base, &off] {
        let r = c
            .get(format!("{b}/api/v1/stats"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_api_headers(r.headers(), "stats");
        assert_eq!(header(r.headers(), "cache-control"), "no-store");
        let r = c.get(format!("{b}/api/v1/stats")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_api_headers(r.headers(), "401");
        assert_eq!(header(r.headers(), "cache-control"), "no-store");
    }
}

#[tokio::test]
async fn login_rules() {
    let Some(db) = test_db().await else { return };
    let base = spawn_web(&db.store, TOKEN, None).await;
    let c = http();
    let url = format!("{base}/api/v1/web/login");

    // Only JSON, only same-origin, small bodies.
    let r = c
        .post(&url)
        .header("content-type", "text/plain")
        .body(format!("{{\"token\":\"{TOKEN}\"}}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let r = c
        .post(&url)
        .header("sec-fetch-site", "cross-site")
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_error(r).await,
        (StatusCode::FORBIDDEN, "forbidden".into())
    );
    let r = c
        .post(&url)
        .json(&json!({"token": TOKEN, "pad": "x".repeat(2000)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // The right token: a cookie for this loopback host, a CSRF token, the ntfy state; the
    // token itself never comes back.
    let r = c
        .post(&url)
        .header("sec-fetch-site", "same-origin")
        .header("user-agent", "webtest/1.0")
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_api_headers(r.headers(), "login");
    assert_eq!(header(r.headers(), "cache-control"), "no-store");
    let set = header(r.headers(), "set-cookie").to_string();
    let (pair, attrs) = set.split_once("; ").unwrap();
    let (name, value) = pair.split_once('=').unwrap();
    // Always `__Host-` and Secure, even on plain http to 127.0.0.1 (D133).
    assert_eq!(name, "__Host-magnetar_session");
    assert_eq!(value.len(), 43);
    assert_eq!(
        attrs,
        "Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=2592000"
    );
    let body = r.text().await.unwrap();
    assert!(!body.contains(TOKEN));
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["csrf"].as_str().unwrap().len(), 43);
    assert_eq!(v["ntfy"], false);
    let exp = v["expires_at"].as_i64().unwrap();
    let now = magnetar_core::now();
    let created = v["created_at"].as_i64().unwrap();
    assert!((now - 5..=now + 5).contains(&created), "{created}");
    assert!((now + 30 * 86400 - 5..=now + 30 * 86400 + 5).contains(&exp));
    let ua: Option<String> = sqlx::query_scalar("SELECT user_agent FROM web_sessions")
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert_eq!(ua.as_deref(), Some("webtest/1.0"));

    // Whatever Host a proxy forwards: `__Host-` and Secure.
    for headers in [
        vec![("host", "example.ts.net")],
        vec![("host", "127.0.0.1:8787")],
        vec![("host", "127.0.0.1:8787"), ("x-forwarded-proto", "https")],
    ] {
        let mut rb = c.post(&url).json(&json!({"token": TOKEN}));
        for (k, v) in &headers {
            rb = rb.header(*k, *v);
        }
        let r = rb.send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{headers:?}");
        let set = header(r.headers(), "set-cookie");
        assert!(set.starts_with("__Host-magnetar_session="), "{set}");
        assert!(
            set.ends_with("; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=2592000"),
            "{set}"
        );
    }

    // Wrong tokens: 401, and after five in a minute 429 with Retry-After, even for the right
    // token (the limit is server-wide).
    for _ in 0..5 {
        let r = c
            .post(&url)
            .json(&json!({"token": "wrong"}))
            .send()
            .await
            .unwrap();
        let body = r.text().await.unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"], "unauthorized");
        assert_eq!(v["message"], "token rejected");
        assert!(!body.contains("wrong"));
    }
    for token in ["wrong", TOKEN] {
        let r = c
            .post(&url)
            .json(&json!({"token": token}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        let wait: u64 = header(r.headers(), "retry-after").parse().unwrap();
        assert!((1..=60).contains(&wait), "{wait}");
        assert_eq!(json_error(r).await.1, "rate_limited");
    }
}

#[tokio::test]
async fn login_reports_ntfy() {
    let Some(db) = test_db().await else { return };
    // MAGNETAR_NTFY_TOPIC set: the Settings page says alerts go out (nothing is sent here).
    let cli = <magnetar_server::config::Cli as clap::Parser>::try_parse_from([
        "magnetar-server",
        "--ntfy-topic",
        "webcheck",
        "serve",
    ])
    .unwrap();
    let web = WebState::from_config(&cli.config);
    let base =
        spawn_state(AppState::new(Arc::new(db.store.clone()), TOKEN).with_web(Some(Arc::new(web))))
            .await;
    let r = http()
        .post(format!("{base}/api/v1/web/login"))
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["ntfy"], true);
    // No env topic, but the `ntfy.topic` setting.
    let base = spawn_web(&db.store, TOKEN, None).await;
    db.store
        .set_setting(magnetar_core::store::keys::NTFY_TOPIC, "\"t\"")
        .await
        .unwrap();
    let b = login(&base, TOKEN).await;
    let r = b
        .req(Method::GET, format!("{base}/api/v1/web/session"))
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["ntfy"], true);
    assert_eq!(v["csrf"], b.csrf);
    assert!(v["created_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn cookie_sessions_and_csrf() {
    let Some(db) = test_db().await else { return };
    // With crawl workers on, so their routes answer 401 (not "workers disabled").
    use magnetar_server::fleet::{Fleet, FleetConfig, parse_tokens};
    let fleet = Fleet::new(
        FleetConfig {
            tokens: parse_tokens("w1:worker-token-0123456789abcdef", Some(TOKEN)).unwrap(),
            lease_secs: 60,
            max_lease: 100,
            metadata_max: 1 << 20,
            fetch_known: false,
            triage_new_per_hour: magnetar_server::fleet::DEFAULT_TRIAGE_NEW_PER_HOUR,
        },
        magnetar_server::dht::screen::global_screen(),
    );
    let web = WebState::new(test_ui(), None);
    let base = spawn_state(
        AppState::new(Arc::new(db.store.clone()), TOKEN)
            .with_fleet(Arc::new(fleet))
            .with_web(Some(Arc::new(web))),
    )
    .await;
    let b = login(&base, TOKEN).await;
    let search = format!("{base}/api/v1/search");

    let r = b.req(Method::GET, search.clone()).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = b
        .req(Method::GET, format!("{base}/api/v1/web/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["csrf"], b.csrf);

    // State changes with the cookie need the CSRF token, from this origin.
    let post = |csrf: Option<&str>, site: Option<&str>| {
        let mut rb = b.req(Method::POST, search.clone()).json(&json!({}));
        if let Some(t) = csrf {
            rb = rb.header("x-csrf-token", t.to_string());
        }
        if let Some(s) = site {
            rb = rb.header("sec-fetch-site", s.to_string());
        }
        rb.send()
    };
    let r = post(None, None).await.unwrap();
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"], "forbidden");
    assert_eq!(body["message"], "missing or bad CSRF token");
    let r = post(Some("nope"), None).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = post(Some(&b.csrf), Some("cross-site")).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = post(Some(&b.csrf), Some("same-site")).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = post(Some(&b.csrf), Some("same-origin")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = post(Some(&b.csrf), None).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // PUT and DELETE too.
    let r = b
        .req(Method::PUT, format!("{base}/api/v1/settings/trust"))
        .json(&json!({"threshold": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // The bearer path is unchanged: no CSRF needed.
    let r = http()
        .post(&search)
        .bearer_auth(TOKEN)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // A wrong bearer token is not rescued by the cookie.
    let r = b
        .req(Method::GET, search.clone())
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Routes the cookie never opens: workers, sync, downloads, the relay.
    for (m, path) in [
        (Method::POST, "/api/v1/worker/heartbeat"),
        (Method::GET, "/api/v1/downloads"),
        (Method::POST, "/api/v1/downloads"),
        (Method::POST, "/api/v1/events/batch"),
        (Method::GET, "/relay"),
    ] {
        let r = b
            .req(m.clone(), format!("{base}{path}"))
            .header("x-csrf-token", &b.csrf)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{m} {path}");
    }

    // Sign out: CSRF required; the cookie is cleared and dead afterwards.
    let logout = format!("{base}/api/v1/web/logout");
    let r = b.req(Method::POST, logout.clone()).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = b
        .req(Method::POST, logout.clone())
        .header("x-csrf-token", &b.csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        header(r.headers(), "set-cookie"),
        "__Host-magnetar_session=; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=0"
    );
    let r = b.req(Method::GET, search.clone()).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = b
        .req(Method::GET, format!("{base}/api/v1/web/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Sign out everywhere: another browser's session dies too.
    let one = login(&base, TOKEN).await;
    let two = login(&base, TOKEN).await;
    for x in [&one, &two] {
        let r = x.req(Method::GET, search.clone()).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
    let r = one
        .req(Method::POST, format!("{logout}?all=1"))
        .header("x-csrf-token", &one.csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    for x in [&one, &two] {
        let r = x.req(Method::GET, search.clone()).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        magnetar_server::web_auth::count(&db.store).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn expired_and_rotated_sessions_die() {
    let Some(db) = test_db().await else { return };
    let base = spawn_web(&db.store, TOKEN, None).await;
    let search = format!("{base}/api/v1/search");

    // An expired row.
    let value = data_encoding::BASE64URL_NOPAD.encode(&[9u8; 32]);
    let id = magnetar_server::web_auth::cookie_id_hash(&value).unwrap();
    let now = magnetar_core::now();
    sqlx::query(
        "INSERT INTO web_sessions (id_hash, csrf, token_fp, created_at, expires_at, last_seen)
         VALUES ($1, 'c', $2, $3, $4, $3)",
    )
    .bind(&id[..])
    .bind(magnetar_server::web_auth::token_fp(TOKEN))
    .bind(now - 40 * 86400)
    .bind(now - 10)
    .execute(db.store.pool())
    .await
    .unwrap();
    let r = http()
        .get(&search)
        .header("cookie", format!("__Host-magnetar_session={value}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // The same row made live again works (so the 401 above was the expiry), but only under
    // the `__Host-` name: a prefix-less cookie could have been planted over plain http.
    sqlx::query("UPDATE web_sessions SET expires_at = $1")
        .bind(now + 100)
        .execute(db.store.pool())
        .await
        .unwrap();
    let r = http()
        .get(&search)
        .header("cookie", format!("magnetar_session={value}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = http()
        .get(&search)
        .header("cookie", format!("__Host-magnetar_session={value}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // A new login clears expired rows.
    sqlx::query("UPDATE web_sessions SET expires_at = $1")
        .bind(now - 1)
        .execute(db.store.pool())
        .await
        .unwrap();
    let b = login(&base, TOKEN).await;
    assert_eq!(
        magnetar_server::web_auth::count(&db.store).await.unwrap(),
        1
    );

    // The API token changes: every session made under the old one is refused.
    let rotated = spawn_web(&db.store, "another-token-456", None).await;
    let r = b
        .req(Method::GET, format!("{rotated}/api/v1/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = b
        .req(Method::GET, format!("{rotated}/api/v1/web/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // Garbage cookies are just "no session".
    for v in ["", "x", "%%%", &"A".repeat(500)] {
        let r = http()
            .get(&search)
            .header(
                "cookie",
                HeaderValue::from_str(&format!("__Host-magnetar_session={v}")).unwrap(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{v}");
    }
}
