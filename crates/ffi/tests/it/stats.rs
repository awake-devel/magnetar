//! Charts and graphs on the phone (D47): local counts offline, server-only series and graphs,
//! the fallback when the server is older than D47.

use crate::common;

use common::*;
use futures::executor::block_on;
use magnetar_core::model::Category;
use magnetar_core::query::SearchQuery;
use magnetar_ffi::*;

fn offline() -> FfiConfig {
    FfiConfig {
        server_url: None,
        api_token: None,
        root_pubkey: None,
        trust_threshold: 0,
        cache_cap_mb: 0,
        seed_relays: vec![],
        download_dir: String::new(),
        state_dir: String::new(),
        show_adult: false,
    }
}

fn corpus(s: &SqliteStore) {
    for (n, title, cat) in [
        (
            1,
            "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW",
            Category::Video,
        ),
        (
            2,
            "Haunted.Mansion.2023.VFQ.720p.WEB-DL.H264-Slay3R",
            Category::Video,
        ),
        (
            3,
            "Fargo.S05.MULTi.1080p.WEB.DDP5.1.x264-FW",
            Category::Video,
        ),
        (4, "Some Book", Category::Book),
    ] {
        torrent(&ih(n), title, &pk(1), 100 + n as i64)
            .with(|p| p.category = cat)
            .put(s);
    }
}

fn fields() -> Vec<String> {
    ["category", "resolution", "language", "genre"]
        .iter()
        .map(|f| f.to_string())
        .collect()
}

fn counts(b: &FfiBreakdown) -> Vec<(&str, u64)> {
    b.counts
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect()
}

#[test]
fn offline_counts_from_the_cache() {
    let dir = TempDir::new("stats");
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), offline()).unwrap();
    corpus(m.store());
    let all: FfiSearchQuery = SearchQuery::default().into();
    let set = block_on(m.stats_breakdown(all, fields(), 1)).unwrap();
    assert_eq!((set.source.as_str(), set.total), ("local", 4));
    assert!(set.server_error.is_none(), "no server asked");
    // Genres need the server's media links: left out.
    let names: Vec<&str> = set.breakdowns.iter().map(|b| b.field.as_str()).collect();
    assert_eq!(names, ["category", "resolution", "language"]);
    let cat = &set.breakdowns[0];
    assert_eq!(counts(cat), [("video", 3)]);
    assert_eq!((cat.other, cat.other_values, cat.missing), (1, 1, 0));
    let res = &set.breakdowns[1];
    assert_eq!(counts(res), [("1080p", 2)]);
    assert_eq!(res.counts[0].label, "1080p");
    assert_eq!((res.other, res.missing), (1, 1), "720p; the book has none");
    let lang = &set.breakdowns[2];
    assert_eq!(lang.missing, 1);
    assert!(lang.counts[0].count >= 2);
    // Series, health and graphs need the server.
    assert!(matches!(
        block_on(m.stats_timeseries(vec!["torrents_added".into()], "day".into(), None)),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        block_on(m.stats_health()),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        block_on(m.graph("wot".into())),
        Err(FfiError::Invalid { .. })
    ));
}

/// A server older than D47 answers 404 for the new routes.
fn spawn_old_server() -> String {
    use axum::routing::get;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let health = || async {
                axum::Json(serde_json::json!({"status":"ok","version":"0.1.0","db":true}))
            };
            let app = axum::Router::new()
                .route("/health", get(health))
                .fallback(|| async {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        axum::Json(
                            serde_json::json!({"error":"not_found","message":"no such route"}),
                        ),
                    )
                });
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

#[test]
fn older_server_falls_back() {
    let dir = TempDir::new("stats-old");
    let mut cfg = offline();
    cfg.server_url = Some(spawn_old_server());
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), cfg).unwrap();
    corpus(m.store());
    let all: FfiSearchQuery = SearchQuery::default().into();
    let set = block_on(m.stats_breakdown(all, fields(), 5)).unwrap();
    assert_eq!(set.source, "local");
    assert!(set.server_error.is_some());
    assert!(block_on(m.graph("relays".into())).unwrap().is_none());
    assert!(matches!(
        block_on(m.stats_health()),
        Err(FfiError::Server { status: 404, .. })
    ));
    assert!(matches!(
        block_on(m.graph("bogus".into())),
        Err(FfiError::Invalid { .. })
    ));
}
