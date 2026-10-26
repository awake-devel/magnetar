//! Seed-check backlog and history from the server (D45): present on a D45 server, None
//! offline and from older servers. Fake servers in-process.

mod common;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::*;
use futures::executor::block_on;
use magnetar_core::api::{LivenessHistory, LivenessPoint, LivenessStats, StatsResponse};
use magnetar_core::liveness::BatchReport;
use magnetar_ffi::*;

const TOKEN: &str = "phone-token";

fn stats(new_server: bool) -> serde_json::Value {
    let mut v = serde_json::to_value(StatsResponse {
        liveness: Some(LivenessStats {
            enabled: true,
            torrents: 500,
            never_checked: 120,
            due_now: 130,
            checks_per_hour: 6000,
            alive: 300,
            last_batch_at: Some(99),
            last_batch: Some(BatchReport {
                hashes: 2000,
                ms: 61_000,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    if !new_server {
        // What a server older than D45 sends: no `liveness` at all.
        v.as_object_mut().unwrap().remove("liveness");
    }
    v
}

async fn history(Path(ih_param): Path<String>) -> Response {
    if ih_param != ih(1) {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error":"not_found","message":"unknown infohash"})),
        )
            .into_response();
    }
    axum::Json(LivenessHistory {
        infohash: ih(1),
        points: vec![LivenessPoint {
            at: 10,
            seeders: Some(4),
            leechers: Some(2),
            peers_est: None,
        }],
        keep_days: 90,
    })
    .into_response()
}

/// Fake server on its own runtime thread; returns its base URL.
fn spawn_fake(new_server: bool) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut app = axum::Router::new().route(
                "/api/v1/stats",
                get(move || async move { axum::Json(stats(new_server)) }),
            );
            if new_server {
                app = app.route("/api/v1/torrents/{ih}/liveness-history", get(history));
            }
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn config(server: Option<String>) -> FfiConfig {
    FfiConfig {
        api_token: server.as_ref().map(|_| TOKEN.to_string()),
        server_url: server,
        root_pubkey: None,
        trust_threshold: 0,
        cache_cap_mb: 0,
        seed_relays: vec![],
        download_dir: String::new(),
        state_dir: String::new(),
    }
}

fn open(tag: &str, server: Option<String>) -> (TempDir, std::sync::Arc<Magnetar>) {
    let dir = TempDir::new(tag);
    let db = dir.0.join("db").join("magnetar.db").display().to_string();
    let m = Magnetar::open(db, config(server)).unwrap();
    (dir, m)
}

#[test]
fn backlog_and_history_from_a_d45_server() {
    let (_dir, m) = open("live-new", Some(spawn_fake(true)));
    let s = block_on(m.server_liveness()).expect("stats from a D45 server");
    assert_eq!((s.torrents, s.never_checked, s.due_now), (500, 120, 130));
    assert_eq!((s.checks_per_hour, s.alive), (6000, 300));
    assert_eq!(
        (s.last_batch_at, s.last_batch_size, s.last_batch_ms),
        (Some(99), 2000, 61_000)
    );

    let h = block_on(m.liveness_history(ih(1), 30)).unwrap().unwrap();
    assert_eq!(h.points.len(), 1);
    assert_eq!(h.points[0].seeders, Some(4));
    assert_eq!(h.keep_days, 90);
    assert!(block_on(m.liveness_history(ih(2), 30)).unwrap().is_none());
    assert!(block_on(m.liveness_history("nope".into(), 30)).is_err());
}

#[test]
fn older_server_and_offline_have_none() {
    let (_dir, m) = open("live-old", Some(spawn_fake(false)));
    assert!(block_on(m.server_liveness()).is_none());
    assert!(block_on(m.liveness_history(ih(1), 30)).unwrap().is_none());

    let (_dir, m) = open("live-offline", None);
    assert!(block_on(m.server_liveness()).is_none());
    assert!(block_on(m.liveness_history(ih(1), 30)).unwrap().is_none());
}
