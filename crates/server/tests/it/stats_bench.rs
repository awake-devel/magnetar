//! Chart and graph endpoints on a production-sized database (D47, numbers in docs/STATS.md).
//! Ignored: needs a database made by `scripts/bench-search-db.sh 1` (a production copy) plus
//! the trust/relay tables, and a release build:
//!
//!   MAGNETAR_BENCH_DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/mag_night_stats \
//!     cargo test --release -p magnetar-server --test it stats_bench:: -- --ignored --nocapture
//!
//! `MAGNETAR_BENCH_INDEX_DIR` keeps the index between runs. Every endpoint is timed with an
//! empty cache (what the first viewer waits for) and must stay under 200 ms. The trust root is
//! set to the most-following account for the web-of-trust run and removed afterwards.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use magnetar_core::api::BreakdownRequest;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::{Store, keys};
use magnetar_server::PgStore;
use magnetar_server::graphs;
use magnetar_server::liveness::LivenessService;
use magnetar_server::search_index::{self, SearchIndex, load};
use magnetar_server::stats::{self, StatsService};

const RUNS: usize = 10;
const BOUND: Duration = Duration::from_millis(200);

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Median and max of `RUNS` cold runs.
async fn time<F, Fut, T>(name: &str, mut f: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut t = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let s = Instant::now();
        let _ = f().await;
        t.push(s.elapsed());
    }
    t.sort();
    let (p50, max) = (t[RUNS / 2], t[RUNS - 1]);
    eprintln!("{name:<44} p50 {:7.1} ms   max {:7.1} ms", ms(p50), ms(max));
    p50
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn stats_at_scale() {
    let Ok(url) = std::env::var("MAGNETAR_BENCH_DATABASE_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_BENCH_DATABASE_URL)");
        return;
    };
    let store = PgStore::connect(&url, 10).await.unwrap();
    store.migrate().await.unwrap();
    let pool = store.pool().clone();
    let id = load::database_id(&pool).await.unwrap();
    let (dir, keep) = match std::env::var_os("MAGNETAR_BENCH_INDEX_DIR") {
        Some(d) => (PathBuf::from(d), true),
        None => (
            std::env::temp_dir().join(format!("magnetar-sbench-{:016x}", rand::random::<u64>())),
            false,
        ),
    };
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    if !idx.is_ready() {
        let t = Instant::now();
        let n = search_index::build(&pool, &idx).await.unwrap();
        eprintln!(
            "index build: {n} documents in {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    store.attach_search_index(idx.clone());
    let now = magnetar_core::now();
    eprintln!(
        "database: {} infohashes",
        load::infohash_count(&pool).await.unwrap()
    );

    // Daemon work (not on a request path).
    let t = Instant::now();
    let b = stats::backfill(&pool).await.unwrap();
    eprintln!("backfill: {b:?} in {:.0} ms", ms(t.elapsed()));
    let t = Instant::now();
    stats::recompute_published(&pool).await.unwrap();
    eprintln!("recompute torrents_published: {:.0} ms", ms(t.elapsed()));
    let t = Instant::now();
    stats::sample_gauges(&pool, now).await.unwrap();
    eprintln!("sample gauges: {:.0} ms", ms(t.elapsed()));

    let mut worst = Duration::ZERO;
    let fields: Vec<String> = [
        "language",
        "resolution",
        "category",
        "source",
        "codec",
        "hdr",
        "genre",
        "year",
        "media_kind",
        "audio",
        "subcategory",
    ]
    .iter()
    .map(|f| f.to_string())
    .collect();
    let cases: Vec<(&str, SearchQuery)> = vec![
        ("breakdown: 11 fields, everything", SearchQuery::default()),
        (
            "breakdown: 11 fields, lang=fr",
            SearchQuery {
                languages: vec!["fr".into()],
                ..Default::default()
            },
        ),
        (
            "breakdown: 11 fields, fr + 1080p + video",
            SearchQuery {
                languages: vec!["fr".into()],
                resolutions: vec!["1080p".into()],
                categories: vec![magnetar_core::model::Category::Video],
                ..Default::default()
            },
        ),
        (
            "breakdown: 11 fields, text \"2023\"",
            SearchQuery {
                text: "2023".into(),
                ..Default::default()
            },
        ),
    ];
    for (name, q) in cases {
        let r = BreakdownRequest {
            query: q,
            fields: fields.clone(),
            top: 12,
        };
        let d = time(name, || async {
            let svc = StatsService::default();
            stats::breakdown(&store, &svc, &r, now)
                .await
                .unwrap()
                .unwrap()
        })
        .await;
        worst = worst.max(d);
    }
    let metrics: Vec<String> = stats::METRICS.iter().map(|m| m.to_string()).collect();
    for (bucket, since) in [("hour", None), ("day", None), ("day", Some(0))] {
        let d = time(
            &format!("timeseries: all metrics, {bucket} {since:?}"),
            || async {
                let svc = StatsService::default();
                stats::timeseries(&pool, &svc, &metrics, bucket, since, now)
                    .await
                    .unwrap()
            },
        )
        .await;
        worst = worst.max(d);
    }
    let live = LivenessService::default();
    let crawler = std::sync::Mutex::default();
    let d = time("health", || async {
        let svc = StatsService::default();
        stats::health(&store, &svc, &live, &crawler, now)
            .await
            .unwrap()
    })
    .await;
    worst = worst.max(d);

    let d = time("graph: wot (no root)", || graphs::wot(&store)).await;
    worst = worst.max(d);
    let root: Option<String> = sqlx::query_scalar(
        "SELECT follower FROM follows GROUP BY follower ORDER BY count(*) DESC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let old_root = store.get_setting(keys::TRUST_ROOT).await.unwrap();
    if let Some(r) = &root {
        store
            .set_setting(keys::TRUST_ROOT, &serde_json::to_string(r).unwrap())
            .await
            .unwrap();
        let g = graphs::wot(&store).await.unwrap();
        eprintln!(
            "wot with root: {} nodes, {} edges",
            g.nodes.len(),
            g.edges.len()
        );
        let d = time("graph: wot (most-following root)", || graphs::wot(&store)).await;
        worst = worst.max(d);
        match &old_root {
            Some(v) => store.set_setting(keys::TRUST_ROOT, v).await.unwrap(),
            None => {
                sqlx::query("DELETE FROM settings WHERE key = $1")
                    .bind(keys::TRUST_ROOT)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
    }
    let g = graphs::relays(&store, now).await.unwrap();
    eprintln!("relays: {} nodes, {} edges", g.nodes.len(), g.edges.len());
    let d = time("graph: relays", || graphs::relays(&store, now)).await;
    worst = worst.max(d);
    let media: Option<i64> = sqlx::query_scalar(
        "SELECT media_id FROM torrent_media GROUP BY media_id ORDER BY count(*) DESC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    if let Some(m) = media {
        let g = graphs::media(&store, m, now).await.unwrap().unwrap();
        eprintln!(
            "media {m}: {} nodes, {} edges",
            g.nodes.len(),
            g.edges.len()
        );
        let d = time("graph: media (most releases)", || {
            graphs::media(&store, m, now)
        })
        .await;
        worst = worst.max(d);
    }
    let ih: String = sqlx::query_scalar(
        "SELECT infohash FROM torrent_infohashes ORDER BY events DESC, infohash LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let d = time("graph: swarm (most events)", || {
        graphs::swarm(&store, &live, &ih, now)
    })
    .await;
    worst = worst.max(d);
    if !keep {
        drop(idx);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(worst < BOUND, "slowest endpoint {:.1} ms", ms(worst));
}
