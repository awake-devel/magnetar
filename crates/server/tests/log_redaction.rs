//! D113: the API token given as `/relay?token=` never reaches the log, at any level (the
//! request span used to record the full URI at DEBUG). Its own test binary: it installs a
//! process-wide TRACE subscriber, which tests running alongside in one process would disturb
//! (tracing caches per callsite whether anyone listens). Loopback only, no database.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_server::{AppState, PgStore, router};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn relay_query_token_is_not_logged() {
    let out = Captured::default();
    let w = out.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .init();
    // A database that is never reached: auth answers before any query.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    let token = "the-real-api-token-0123456789";
    let state = AppState::new(Arc::new(PgStore::from_pool(pool)), token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    // A wrong token and the right one (no WebSocket upgrade: a 4xx either way).
    for t in ["a-wrong-token-9876543210", token] {
        let r = http
            .get(format!("http://{addr}/relay?a=1&token={t}"))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_client_error(), "{}", r.status());
    }
    let log = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("/relay?a=1&token="),
        "the request was traced: {log}"
    );
    assert!(!log.contains(token), "token in the log: {log}");
    assert!(!log.contains("a-wrong-token"), "token in the log: {log}");
}
