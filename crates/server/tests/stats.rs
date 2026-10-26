//! Charts and graphs against Postgres (D47): breakdowns through the index, activity series,
//! health, the four graphs and their API. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use magnetar_core::api::*;
use magnetar_core::model::*;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::{Store, keys};
use magnetar_server::PgStore;
use magnetar_server::search_index::{self, SearchIndex, load};
use magnetar_server::stats::{self, StatsService};

/// Keeps the index attached for the test and removes its directory afterwards.
struct Ix {
    _idx: Arc<SearchIndex>,
    dir: PathBuf,
}

impl Drop for Ix {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn index_for(store: &PgStore) -> Ix {
    let dir = std::env::temp_dir().join(format!("magnetar-st-{:016x}", rand::random::<u64>()));
    let id = load::database_id(store.pool()).await.unwrap();
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    search_index::build(store.pool(), &idx).await.unwrap();
    store.attach_search_index(idx.clone());
    Ix { _idx: idx, dir }
}

async fn media(store: &PgStore, kind: &str, title: &str, year: i32) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO media (kind, title, year, genres, updated_at)
         VALUES ($1, $2, $3, '{Drama}', 0) RETURNING id",
    )
    .bind(kind)
    .bind(title)
    .bind(year)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn link(store: &PgStore, infohash: u64, media_id: i64) {
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
         VALUES ($1, $2, 'manual', 1.0, 1, 0)",
    )
    .bind(ih(infohash))
    .bind(media_id)
    .execute(store.pool())
    .await
    .unwrap();
}

/// French and English releases of one film by two publishers, plus an anime and a book.
async fn corpus(s: &PgStore) -> i64 {
    let film = media(s, "movie", "Haunted Mansion", 2023).await;
    let rows: &[(u64, u64, &str, Category)] = &[
        (
            1,
            1,
            "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW",
            Category::Video,
        ),
        (
            2,
            1,
            "Haunted.Mansion.2023.VFQ.720p.WEB-DL.H264-Slay3R",
            Category::Video,
        ),
        (
            3,
            2,
            "Haunted.Mansion.2023.TRUEFRENCH.2160p.BluRay.REMUX.HEVC-GRP",
            Category::Video,
        ),
        (
            4,
            2,
            "Haunted.Mansion.2023.1080p.WEB.H264-FW",
            Category::Video,
        ),
        (
            5,
            2,
            "[SubsPlease] Toumei Otoko - 07 (1080p) [4874FF4E].mkv",
            Category::Anime,
        ),
        (6, 3, "Some Book", Category::Book),
    ];
    for (n, pk, title, cat) in rows {
        put(
            s,
            torrent(*n, *pk, NOW - *n as i64 * 10, *n, title).cat(*cat),
        )
        .await;
    }
    for n in [1, 2, 3, 4] {
        link(s, n, film).await;
    }
    film
}

fn q() -> SearchQuery {
    SearchQuery {
        min_trust: Some(0),
        ..Default::default()
    }
}

fn counts(b: &Breakdown) -> Vec<(&str, u64)> {
    b.counts
        .iter()
        .map(|c| (c.value.as_str(), c.count))
        .collect()
}

#[tokio::test]
async fn breakdowns_cross_filter() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let svc = StatsService::default();
    let req = BreakdownRequest {
        query: q(),
        fields: vec!["resolution".into(), "category".into(), "language".into()],
        top: 2,
    };
    // No index yet: nothing to count with.
    assert!(
        stats::breakdown(s, &svc, &req, NOW)
            .await
            .unwrap()
            .is_none()
    );
    let _ix = index_for(s).await;
    let set = stats::breakdown(s, &svc, &req, NOW).await.unwrap().unwrap();
    assert_eq!(set.total, 6);
    let res = &set.breakdowns[0];
    assert_eq!(counts(res), vec![("1080p", 3), ("2160p", 1)]);
    assert_eq!(
        (res.other, res.other_values, res.missing),
        (1, 1, 1),
        "720p, and the book"
    );
    assert_eq!(counts(&set.breakdowns[1]), vec![("video", 4), ("anime", 1)]);
    assert_eq!(set.breakdowns[1].other, 1);
    // Cross-filter on French: the other fields shrink, language keeps its own values.
    let fr = BreakdownRequest {
        query: SearchQuery {
            languages: vec!["fr".into()],
            ..q()
        },
        top: 10,
        ..req.clone()
    };
    let set = stats::breakdown(s, &svc, &fr, NOW).await.unwrap().unwrap();
    assert_eq!(set.total, 3);
    let res = &set.breakdowns[0];
    assert_eq!(counts(res), vec![("1080p", 1), ("2160p", 1), ("720p", 1)]);
    assert_eq!(res.missing, 0);
    let lang = &set.breakdowns[2];
    assert_eq!(lang.total, 6, "own filter left out");
    assert!(lang.counts.iter().any(|c| c.value == "fr" && c.count == 3));
    assert!(
        stats::breakdown(
            s,
            &svc,
            &BreakdownRequest {
                fields: vec!["nope".into()],
                ..fr
            },
            NOW
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn series_backfill_and_gauges() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let now = magnetar_core::now();
    corpus(s).await;
    // A second event for a known infohash is not a new torrent.
    put(s, torrent(60, 9, NOW, 1, "again")).await;
    s.upsert_relay("wss://a.example", "seed").await.unwrap();
    s.upsert_relay("wss://b.example", "nip65").await.unwrap();
    s.upsert_relay("wss://a.example", "manual").await.unwrap();
    stats::bump(pool, stats::CRAWL_EVENTS, 41).await;
    stats::bump(pool, stats::CRAWL_EVENTS, 1).await;
    stats::sample_gauges(pool, now).await.unwrap();
    stats::recompute_published(pool).await.unwrap();
    let svc = StatsService::default();
    let metrics: Vec<String> = stats::METRICS.iter().map(|m| m.to_string()).collect();
    let set = stats::timeseries(pool, &svc, &metrics, "hour", None, now)
        .await
        .unwrap();
    let get = |m: &str| set.series.iter().find(|x| x.metric == m).unwrap();
    let last = |m: &str| get(m).points.last().unwrap().value;
    assert_eq!(
        get("torrents_added").points.len(),
        49,
        "48 h + the current hour"
    );
    assert_eq!(last("torrents_added"), 6);
    assert_eq!(last("torrents_total"), 6);
    assert_eq!(get("torrents_total").points[0].value, 0);
    assert_eq!(
        last("relays_discovered"),
        2,
        "the manual re-add is no discovery"
    );
    assert_eq!(last("crawl_events"), 42);
    assert!(last("db_size") > 0);
    assert_eq!(get("db_size").kind, "gauge");
    let published = get("torrents_published");
    assert_eq!(published.bucket, "day");
    // NOW is in the past of the test's window: published days are outside it.
    let all = stats::timeseries(
        pool,
        &svc,
        &["torrents_published".into()],
        "day",
        Some(0),
        now,
    )
    .await
    .unwrap();
    assert_eq!(all.series[0].points.iter().map(|p| p.value).sum::<i64>(), 6);
    assert!(
        stats::timeseries(pool, &svc, &["nope".into()], "day", None, now)
            .await
            .is_err()
    );
    assert!(
        stats::timeseries(pool, &svc, &metrics, "week", None, now)
            .await
            .is_err()
    );

    // Rows from before the counters: the one-off backfill from `received_at`.
    sqlx::query("DELETE FROM stats_series")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE meta SET value = $1 WHERE key = 'stats_since'")
        .bind(now + 3600)
        .execute(pool)
        .await
        .unwrap();
    assert!(stats::backfill(pool).await.unwrap().is_some());
    assert_eq!(stats::backfill(pool).await.unwrap(), None, "only once");
    let n: i64 = sqlx::query_scalar(
        "SELECT sum(value)::bigint FROM stats_series WHERE metric = 'torrents_added'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 6);
    assert!(
        stats::prune(pool, now + 2 * stats::KEEP_DAYS * 86400)
            .await
            .unwrap()
            >= 1
    );
}

#[tokio::test]
async fn graphs() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let film = corpus(s).await;
    s.set_setting(keys::TRUST_THRESHOLD, "0").await.unwrap();
    let svc = magnetar_server::liveness::LivenessService::default();

    // No root: publishers and who of them follows whom.
    sqlx::query("INSERT INTO follows VALUES ($1, $2)")
        .bind(hex64(1))
        .bind(hex64(2))
        .execute(pool)
        .await
        .unwrap();
    let g = magnetar_server::graphs::wot(s).await.unwrap();
    assert_eq!(g.nodes.len(), 3);
    assert_eq!(g.edges.len(), 1);
    assert!(g.note.is_some());

    // Root 100 follows 101 (who follows publisher 1) and publisher 2 directly.
    let root = hex64(100);
    s.set_setting(keys::TRUST_ROOT, &serde_json::to_string(&root).unwrap())
        .await
        .unwrap();
    for (a, b) in [(100, 101), (100, 2), (101, 1), (100, 555)] {
        sqlx::query("INSERT INTO follows VALUES ($1, $2)")
            .bind(hex64(a))
            .bind(hex64(b))
            .execute(pool)
            .await
            .unwrap();
    }
    let g = magnetar_server::graphs::wot(s).await.unwrap();
    let id = |k: &str| g.nodes.iter().position(|n| n.id == k).unwrap() as u32;
    assert_eq!(g.nodes[0].kind, "root");
    assert_eq!(g.nodes[id(&hex64(101)) as usize].kind, "follow");
    assert!(
        g.nodes.iter().all(|n| n.id != hex64(555)),
        "follows reaching no publisher left out"
    );
    let has = |a: u32, b: u32| g.edges.iter().any(|e| e.source == a && e.target == b);
    assert!(has(id(&root), id(&hex64(101))));
    assert!(has(id(&hex64(101)), id(&hex64(1))));
    assert!(has(id(&root), id(&hex64(2))));
    assert!(has(id(&hex64(1)), id(&hex64(2))));

    // Relays: NIP-65 write relays of the publishers.
    for (pk, url, write) in [
        (1, "wss://r1", true),
        (2, "wss://r1", true),
        (2, "wss://r2", false),
    ] {
        sqlx::query("INSERT INTO relay_lists VALUES ($1, $2, true, $3)")
            .bind(hex64(pk))
            .bind(url)
            .bind(write)
            .execute(pool)
            .await
            .unwrap();
    }
    let g = magnetar_server::graphs::relays(s, NOW).await.unwrap();
    assert_eq!(g.nodes.iter().filter(|n| n.kind == "relay").count(), 1);
    assert_eq!(g.edges.len(), 2);
    assert!(g.note.as_deref().unwrap().contains("1 of the top 3"));

    // One title: qualities, releases, languages, groups.
    let g = magnetar_server::graphs::media(s, film, NOW)
        .await
        .unwrap()
        .unwrap();
    let kinds = |k: &str| g.nodes.iter().filter(|n| n.kind == k).count();
    assert_eq!(
        (kinds("media"), kinds("release"), kinds("quality")),
        (1, 4, 3)
    );
    assert!(kinds("language") >= 2 && kinds("group") == 3);
    assert!(
        magnetar_server::graphs::media(s, film + 99, NOW)
            .await
            .unwrap()
            .is_none()
    );

    // Swarm: trackers, DHT, publishers.
    let l = Liveness {
        infohash: ih(1),
        seeders: Some(12),
        leechers: Some(3),
        completed: None,
        dht_peers_est: Some(20),
        source: LivenessSource::Udp,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at: NOW,
        expires_at: NOW + 3600,
    };
    s.put_liveness(&l).await.unwrap();
    put(
        s,
        torrent(
            61,
            7,
            NOW,
            1,
            "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW",
        ),
    )
    .await;
    let g = magnetar_server::graphs::swarm(s, &svc, &ih(1), NOW)
        .await
        .unwrap()
        .unwrap();
    let kinds = |k: &str| g.nodes.iter().filter(|n| n.kind == k).count();
    assert_eq!(
        (
            kinds("torrent"),
            kinds("tracker"),
            kinds("dht"),
            kinds("publisher")
        ),
        (1, 1, 1, 2)
    );
    assert!(g.nodes[0].detail.iter().any(|d| d.contains("12 seeders")));
    assert!(
        magnetar_server::graphs::swarm(s, &svc, &ih(999), NOW)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn api() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let film = corpus(s).await;
    let base = spawn_server(s).await;
    let api = ApiClient::new(&base, Some(TOKEN.into())).unwrap();
    let health = api.health().await.unwrap();
    assert!(health.features.iter().any(|f| f == features::STATS));
    assert!(health.features.iter().any(|f| f == features::GRAPHS));
    let r = BreakdownRequest {
        query: q(),
        fields: vec!["language".into()],
        top: 5,
    };
    match api.stats_breakdown(&r).await {
        Err(magnetar_core::Error::Server { status: 503, .. }) => {}
        other => panic!("index not built: {other:?}"),
    }
    let _ix = index_for(s).await;
    let set = api.stats_breakdown(&r).await.unwrap();
    assert_eq!(set.total, 6);
    let http = reqwest::Client::new();
    let get = |path: String| {
        let http = http.clone();
        async move { http.get(path).bearer_auth(TOKEN).send().await.unwrap() }
    };
    let set: BreakdownSet = get(format!(
        "{base}/api/v1/stats/breakdown?field=resolution,category&field=year&top=3&min_trust=0&lang=fr"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(set.breakdowns.len(), 3);
    assert_eq!(set.total, 3);
    assert_eq!(
        get(format!("{base}/api/v1/stats/breakdown?field=x"))
            .await
            .status(),
        400
    );
    let ts = api
        .stats_timeseries(
            &["torrents_added".into(), "torrents_total".into()],
            "day",
            None,
        )
        .await
        .unwrap();
    assert_eq!(ts.series.len(), 2);
    assert_eq!(ts.series[0].points.len(), 91);
    assert_eq!(
        get(format!("{base}/api/v1/stats/timeseries?metric=x"))
            .await
            .status(),
        400
    );
    let h = api.stats_health().await.unwrap();
    assert_eq!(h.torrents, 6);
    assert_eq!(h.media_linked, 4);
    assert!(h.liveness.is_some());
    assert!(api.graph("wot").await.unwrap().is_some());
    assert!(api.graph("relays").await.unwrap().is_some());
    assert!(api.graph(&format!("media/{film}")).await.unwrap().is_some());
    assert!(
        api.graph(&format!("swarm/{}", ih(1)))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        api.graph(&format!("swarm/{}", ih(77)))
            .await
            .unwrap()
            .is_none()
    );
    assert!(api.graph("media/12345").await.unwrap().is_none());
    assert!(api.graph("nope").await.is_err());
    assert_eq!(
        get(format!("{base}/api/v1/graphs/swarm/xyz"))
            .await
            .status(),
        400
    );
    assert_eq!(
        http.get(format!("{base}/api/v1/stats/health"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}
