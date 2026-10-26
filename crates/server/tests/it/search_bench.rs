//! Search index benchmark at production scale (D43, numbers in docs/SEARCH.md). Ignored: it
//! needs a database made by `scripts/bench-search-db.sh` (~3.2 M infohashes) and a release build:
//!
//!   MAGNETAR_BENCH_DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/mag_night_bench \
//!     cargo test --release -p magnetar-server --test it search_bench:: -- --ignored --nocapture
//!
//! `MAGNETAR_BENCH_INDEX_DIR` keeps the index between runs (default: a temporary directory,
//! removed afterwards). `MAGNETAR_BENCH_PG=1` also times the Postgres search once for a few of
//! the queries (seconds to minutes each at this size). Every query must stay under 100 ms server-side.

use crate::common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::search_query_set;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::search_index::{self, SearchIndex, load};

const RUNS: usize = 25;
const BOUND: Duration = Duration::from_millis(100);

fn proc_status(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .map(|l| l[key.len()..].trim().to_string())
        })
        .unwrap_or_default()
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

async fn time_query(store: &PgStore, q: &SearchQuery, now: i64) -> (Duration, u64, bool) {
    let t = Instant::now();
    let p = store.search(q, now).await.unwrap();
    (t.elapsed(), p.total, p.total_is_lower_bound)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn search_at_scale() {
    let Ok(url) = std::env::var("MAGNETAR_BENCH_DATABASE_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_BENCH_DATABASE_URL)");
        return;
    };
    let store = PgStore::connect(&url, 10).await.unwrap();
    let pool = store.pool().clone();
    let id = load::database_id(&pool).await.unwrap();
    let (dir, keep) = match std::env::var_os("MAGNETAR_BENCH_INDEX_DIR") {
        Some(d) => (PathBuf::from(d), true),
        None => (
            std::env::temp_dir().join(format!("magnetar-bench-{:016x}", rand::random::<u64>())),
            false,
        ),
    };
    let infohashes = load::infohash_count(&pool).await.unwrap();
    eprintln!(
        "database: {infohashes} infohashes; index in {}",
        dir.display()
    );

    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    if !idx.is_ready() {
        let t = Instant::now();
        let docs = search_index::build(&pool, &idx).await.unwrap();
        eprintln!(
            "build: {docs} documents in {:.1} s ({:.0} docs/s)",
            t.elapsed().as_secs_f64(),
            docs as f64 / t.elapsed().as_secs_f64()
        );
    }
    store.attach_search_index(idx.clone());
    let st = store.search_index_status().await.unwrap();
    eprintln!(
        "index: {} docs, {:.0} MiB on disk; process RSS {} (peak {}; anonymous {}, mapped files {})",
        st.docs,
        st.bytes as f64 / (1 << 20) as f64,
        proc_status("VmRSS:"),
        proc_status("VmHWM:"),
        proc_status("RssAnon:"),
        proc_status("RssFile:")
    );

    let now = magnetar_core::now();
    let set = search_query_set(now);
    // Warm the page cache and the term dictionaries once.
    for (_, q) in &set {
        time_query(&store, q, now).await;
    }
    eprintln!(
        "\n{:<44} {:>9} {:>9} {:>9} {:>11}",
        "query", "p50 ms", "p95 ms", "max ms", "total"
    );
    let mut all: Vec<Duration> = Vec::new();
    let mut worst: Vec<(String, Duration)> = Vec::new();
    for (name, q) in &set {
        let mut samples = Vec::with_capacity(RUNS);
        let mut total = (0, false);
        for _ in 0..RUNS {
            let (d, n, lb) = time_query(&store, q, now).await;
            samples.push(d);
            total = (n, lb);
        }
        samples.sort();
        let max = *samples.last().unwrap();
        eprintln!(
            "{:<44} {:>9.2} {:>9.2} {:>9.2} {:>10}{}",
            name,
            ms(pct(&samples, 0.5)),
            ms(pct(&samples, 0.95)),
            ms(max),
            total.0,
            if total.1 { "+" } else { "" }
        );
        all.extend(&samples);
        worst.push((name.clone(), max));
    }
    all.sort();
    eprintln!(
        "{:<44} {:>9.2} {:>9.2} {:>9.2}",
        "ALL",
        ms(pct(&all, 0.5)),
        ms(pct(&all, 0.95)),
        ms(*all.last().unwrap())
    );

    // Facets over the whole index and over a text query.
    for (field, text) in [("category", ""), ("genre", "french"), ("year", "")] {
        let mut q = SearchQuery {
            text: text.into(),
            min_trust: Some(0),
            ..Default::default()
        };
        q.descending = true;
        let t = Instant::now();
        let f = store.facets(&q, field, 20, now).await.unwrap().unwrap();
        eprintln!(
            "facets {field:<10} {text:<8} {:>8.2} ms  ({} values over {})",
            ms(t.elapsed()),
            f.counts.len(),
            f.total
        );
    }

    // The app's filter sheet (D44): every facet at once, disjunctive, with and without filters.
    let fields: Vec<String> = [
        "language",
        "subtitles",
        "resolution",
        "source",
        "codec",
        "hdr",
        "audio",
        "genre",
        "media_kind",
    ]
    .map(String::from)
    .to_vec();
    for (name, q) in [
        (
            "browse",
            SearchQuery {
                min_trust: Some(0),
                ..Default::default()
            },
        ),
        (
            "\"french\" fr-ca 1080p",
            SearchQuery {
                text: "french".into(),
                languages: vec!["fr-ca".into()],
                resolutions: vec!["1080p".into()],
                min_trust: Some(0),
                ..Default::default()
            },
        ),
    ] {
        let mut samples: Vec<Duration> = Vec::new();
        let mut total = 0;
        for _ in 0..5 {
            let t = Instant::now();
            total = store
                .facet_set(&q, &fields, 50, now)
                .await
                .unwrap()
                .unwrap()
                .total;
            samples.push(t.elapsed());
        }
        samples.sort();
        eprintln!(
            "facet set (9 facets) {name:<22} p50 {:>8.2} ms max {:>8.2} ms ({total} results)",
            ms(pct(&samples, 0.5)),
            ms(*samples.last().unwrap())
        );
    }

    // Throughput: the whole set from 8 concurrent clients.
    let t = Instant::now();
    let store = Arc::new(store);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let set = set.clone();
        tasks.push(tokio::spawn(async move {
            for (_, q) in &set {
                time_query(&store, q, now).await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let n = set.len() * 8;
    eprintln!(
        "8 concurrent clients: {n} queries in {:.2} s ({:.0} q/s)",
        t.elapsed().as_secs_f64(),
        n as f64 / t.elapsed().as_secs_f64()
    );
    eprintln!(
        "process RSS after queries {} (peak {}; anonymous {}, mapped files {})",
        proc_status("VmRSS:"),
        proc_status("VmHWM:"),
        proc_status("RssAnon:"),
        proc_status("RssFile:")
    );

    // One update through the change log, timed end to end (trigger → commit → searchable).
    let ih: String =
        sqlx::query_scalar("SELECT infohash FROM torrent_infohashes ORDER BY infohash LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    let t = Instant::now();
    sqlx::query("UPDATE torrents SET title = title || ' benchupdate' WHERE infohash = $1")
        .bind(&ih)
        .execute(&pool)
        .await
        .unwrap();
    while search_index::apply_changes(&pool, &idx).await.unwrap() > 0 {}
    let q = SearchQuery {
        text: "benchupdate".into(),
        min_trust: Some(0),
        ..Default::default()
    };
    assert!(store.search(&q, now).await.unwrap().total >= 1);
    eprintln!(
        "update applied and searchable in {:.1} ms (without the 1 s poll)",
        ms(t.elapsed())
    );
    sqlx::query(
        "UPDATE torrents SET title = replace(title, ' benchupdate', '') WHERE infohash = $1",
    )
    .bind(&ih)
    .execute(&pool)
    .await
    .unwrap();
    while search_index::apply_changes(&pool, &idx).await.unwrap() > 0 {}

    if std::env::var("MAGNETAR_BENCH_PG").is_ok_and(|v| v == "1") {
        let pg = PgStore::from_pool(pool.clone());
        eprintln!("\nPostgres search (fallback), one run each:");
        let picked = [
            "browse",
            "1 letter \"a\"",
            "common \"1080p\"",
            "multi-word \"the lord of the rings\"",
            "typo \"interstelar\"",
            "sort Seeders desc \"french\"",
        ];
        for (name, q) in set.iter().filter(|(n, _)| picked.contains(&n.as_str())) {
            let t = Instant::now();
            let p = pg.search(q, now).await.unwrap();
            eprintln!(
                "{:<44} {:>10.0} ms {:>10}{}",
                name,
                ms(t.elapsed()),
                p.total,
                if p.total_is_lower_bound { "+" } else { "" }
            );
        }
    }

    drop(idx);
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let over: Vec<_> = worst.iter().filter(|(_, d)| *d >= BOUND).collect();
    assert!(over.is_empty(), "queries over {BOUND:?}: {over:?}");
}
