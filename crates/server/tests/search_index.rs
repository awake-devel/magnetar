//! Tantivy search index (D43) against Postgres: semantics parity with the SQL search, text
//! matching, near-real-time updates, rebuild/resume, facets, counters, latency.
//! Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED), except the in-memory latency test.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::search_index::schema::IndexDoc;
use magnetar_server::search_index::{self, SearchIndex, index_query, load};

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    }
}

fn titles(page: &SearchPage) -> Vec<String> {
    page.hits.iter().map(|h| h.title.clone()).collect()
}

fn live(infohash: u64, seeders: Option<u32>, dht: Option<u32>, expires_at: i64) -> Liveness {
    Liveness {
        infohash: ih(infohash),
        seeders,
        leechers: seeders.map(|_| 1),
        completed: None,
        dht_peers_est: dht,
        source: LivenessSource::Udp,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at: NOW - 10,
        expires_at,
    }
}

fn temp_index_dir() -> PathBuf {
    std::env::temp_dir().join(format!("magnetar-ix-{:016x}", rand::random::<u64>()))
}

/// A built index attached to the test store, removed on drop.
struct Ix {
    idx: Arc<SearchIndex>,
    dir: PathBuf,
}

impl Drop for Ix {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn index_for(store: &PgStore) -> Ix {
    let dir = temp_index_dir();
    let id = load::database_id(store.pool()).await.unwrap();
    assert!(!id.is_empty(), "the migration names the database");
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    assert!(!idx.is_ready());
    search_index::build(store.pool(), &idx).await.unwrap();
    assert!(idx.is_ready());
    store.attach_search_index(idx.clone());
    Ix { idx, dir }
}

/// Apply every queued change.
async fn sync(store: &PgStore, ix: &Ix) {
    while search_index::apply_changes(store.pool(), &ix.idx)
        .await
        .unwrap()
        > 0
    {}
}

/// Same database, no index: the Postgres search.
fn pg_only(store: &PgStore) -> PgStore {
    PgStore::from_pool(store.pool().clone())
}

/// (infohash, title, trust, publisher_count, dead, seeders) of one hit.
type HitShape = (String, String, u8, u32, Option<bool>, Option<u32>);

/// What both searches must agree on (rank and matched_file differ by design).
fn shape(p: &SearchPage) -> (u64, Vec<HitShape>) {
    (
        p.total,
        p.hits
            .iter()
            .map(|h| {
                (
                    h.infohash.clone(),
                    h.title.clone(),
                    h.trust,
                    h.publisher_count,
                    h.dead,
                    h.seeders,
                )
            })
            .collect(),
    )
}

#[tokio::test]
async fn filters_sorts_and_grouping_match_postgres() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 300, 1, "video small")
            .cat(Category::Video)
            .size(100),
    )
    .await;
    put(
        s,
        torrent(2, 1, NOW - 200, 2, "video big")
            .cat(Category::Video)
            .size(10_000),
    )
    .await;
    put(
        s,
        torrent(3, 1, NOW - 100, 3, "audio unknown size").cat(Category::Audio),
    )
    .await;
    put(
        s,
        torrent(4, 2, NOW - 50, 4, "pow bot")
            .cat(Category::Anime)
            .pow(21),
    )
    .await;
    // Same infohash three times: publisher 6 is earliest, 7 has PoW (highest trust).
    put(s, torrent(5, 5, NOW - 10, 7, "copy late")).await;
    put(s, torrent(6, 6, NOW - 60, 7, "copy early")).await;
    put(s, torrent(7, 7, NOW - 5, 7, "copy pow").pow(24)).await;
    for (n, title) in [
        (11, "dead fresh"),
        (12, "dead stale"),
        (13, "alive"),
        (14, "dht only"),
    ] {
        put(s, torrent(n, 3, NOW - n as i64, n, title).size(n * 10)).await;
    }
    s.put_liveness(&live(11, Some(0), None, NOW + 100))
        .await
        .unwrap();
    s.put_liveness(&live(12, Some(0), None, NOW - 1))
        .await
        .unwrap();
    s.put_liveness(&live(13, Some(7), None, NOW + 100))
        .await
        .unwrap();
    s.put_liveness(&live(14, None, Some(3), NOW + 100))
        .await
        .unwrap();
    let ix = index_for(s).await;
    let pg = pg_only(s);
    assert_eq!(ix.idx.num_docs(), 9);

    let mut cases: Vec<SearchQuery> = Vec::new();
    for sort in [Sort::Date, Sort::Size, Sort::Seeders] {
        for desc in [true, false] {
            let mut f = q("");
            f.sort = sort;
            f.descending = desc;
            cases.push(f);
        }
    }
    let mut f = q("");
    f.categories = vec![Category::Video, Category::Audio];
    cases.push(f);
    let mut f = q("");
    f.min_size = Some(500);
    cases.push(f.clone());
    f.min_size = None;
    f.max_size = Some(500);
    cases.push(f);
    // Date, size and category bounds apply to the canonical event in the index, to any event
    // in Postgres (D43); this range does not split the copies of infohash 7.
    let mut f = q("");
    f.since = Some(NOW - 200);
    f.until = Some(NOW - 50);
    f.categories = vec![Category::Video, Category::Audio, Category::Anime];
    cases.push(f);
    let mut f = q("");
    f.hide_dead = true;
    cases.push(f);
    let mut f = q("");
    f.min_seeders = Some(1);
    cases.push(f);
    let mut f = q("");
    f.min_trust = None; // setting default 20: only the PoW events pass
    cases.push(f.clone());
    f.min_trust = Some(21);
    cases.push(f);
    let mut f = q("");
    f.limit = 3;
    f.offset = 2;
    cases.push(f);
    let mut f = q("copy");
    f.sort = Sort::Date;
    cases.push(f);

    for (i, c) in cases.iter().enumerate() {
        let a = s.search(c, NOW).await.unwrap();
        let b = pg.search(c, NOW).await.unwrap();
        assert_eq!(shape(&a), shape(&b), "case {i}: {c:?}");
    }

    let g = s.search(&q("copy"), NOW).await.unwrap();
    assert_eq!(titles(&g), vec!["copy pow"], "highest trust wins");
    assert_eq!(g.hits[0].publisher_count, 3);
    assert_eq!(g.hits[0].npub, npub(&hex64(7)));

    // Overrides are baked into the documents: block removes that publisher's events (the
    // earliest remaining event becomes canonical), trust passes any threshold.
    s.set_override(&hex64(7), Some(OverrideAction::Block))
        .await
        .unwrap();
    s.set_override(&hex64(1), Some(OverrideAction::Trust))
        .await
        .unwrap();
    sync(s, &ix).await;
    let mut f = q("");
    f.min_trust = Some(90);
    for c in [q("copy"), q(""), f] {
        assert_eq!(
            shape(&s.search(&c, NOW).await.unwrap()),
            shape(&pg.search(&c, NOW).await.unwrap()),
            "{c:?}"
        );
    }
    let g = s.search(&q("copy"), NOW).await.unwrap();
    assert_eq!(titles(&g), vec!["copy early"]);
    assert_eq!(g.hits[0].publisher_count, 2);
    s.set_override(&hex64(7), None).await.unwrap();
    sync(s, &ix).await;
    assert_eq!(
        titles(&s.search(&q("copy"), NOW).await.unwrap()),
        vec!["copy pow"]
    );
}

#[tokio::test]
async fn text_matching() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 100, 1, "Big.Buck.Bunny.2008.1080p.x264-GRP"),
    )
    .await;
    put(s, torrent(2, 1, NOW - 90, 2, "Ubuntu 24.04 desktop amd64")).await;
    put(
        s,
        torrent(3, 1, NOW - 80, 3, "Unrelated").desc("A story about rabbits and carrots"),
    )
    .await;
    put(
        s,
        torrent(4, 1, NOW - 70, 4, "Pack").files(&[
            ("Season.01/Show.S01E01.mkv", 1),
            ("Season.01/Show.S01E02.mkv", 1),
        ]),
    )
    .await;
    put(
        s,
        torrent(5, 1, NOW - 60, 5, "L'Été.Meurtrier.1983.FRENCH.1080p"),
    )
    .await;
    put(s, torrent(6, 1, NOW - 50, 6, "The Lord of the Rings")).await;
    put(s, torrent(7, 1, NOW - 40, 7, "Le Seigneur des Anneaux")).await;
    put(
        s,
        torrent(8, 1, NOW - 30, 8, "Interstellar.2014.2160p.REMUX"),
    )
    .await;
    // "bunny" only in a file name: ranks below the title hit, says which file matched.
    put(
        s,
        torrent(9, 1, NOW - 20, 9, "Blender open movies").files(&[
            ("docs/readme.txt", 1),
            ("Open.Movies/Big.Buck.Bunny.mp4", 1),
        ]),
    )
    .await;
    let _ix = index_for(s).await;

    let page = s.search(&q("bunny 1080p"), NOW).await.unwrap();
    assert_eq!(titles(&page), vec!["Big.Buck.Bunny.2008.1080p.x264-GRP"]);
    assert_eq!((page.total, page.total_is_lower_bound), (1, false));
    assert_eq!(page.hits[0].matched_file, None);

    let page = s.search(&q("big buck bun"), NOW).await.unwrap();
    assert_eq!(
        titles(&page),
        vec!["Big.Buck.Bunny.2008.1080p.x264-GRP", "Blender open movies"],
        "title hits rank above file-only hits"
    );
    assert_eq!(
        page.hits[1].matched_file.as_deref(),
        Some("Open.Movies/Big.Buck.Bunny.mp4")
    );
    assert!(page.hits[0].rank > page.hits[1].rank);

    // Typos: distance 1 from 5 letters, 2 from 8; none on short words or words with digits.
    assert_eq!(
        titles(&s.search(&q("ubunto"), NOW).await.unwrap()),
        vec!["Ubuntu 24.04 desktop amd64"]
    );
    assert_eq!(
        titles(&s.search(&q("intersteller"), NOW).await.unwrap()),
        vec!["Interstellar.2014.2160p.REMUX"]
    );
    assert_eq!(
        titles(&s.search(&q("instelar"), NOW).await.unwrap()),
        Vec::<String>::new()
    );
    assert_eq!(s.search(&q("buk"), NOW).await.unwrap().total, 0);
    assert_eq!(s.search(&q("2009"), NOW).await.unwrap().total, 0);
    // Prefix on the last word only, from 3 letters.
    assert_eq!(s.search(&q("ubu"), NOW).await.unwrap().total, 1);
    assert_eq!(s.search(&q("ub"), NOW).await.unwrap().total, 0);
    assert_eq!(s.search(&q("ubu desktop"), NOW).await.unwrap().total, 0);
    // Description (lower weight), with the last word as a prefix.
    assert_eq!(
        titles(&s.search(&q("rabbit"), NOW).await.unwrap()),
        vec!["Unrelated"]
    );
    // File names, matched file reported.
    let page = s.search(&q("s01e02"), NOW).await.unwrap();
    assert_eq!(titles(&page), vec!["Pack"]);
    assert_eq!(
        page.hits[0].matched_file.as_deref(),
        Some("Season.01/Show.S01E02.mkv")
    );
    // Accent and case folding.
    for t in ["ete meurtrier", "ÉTÉ", "été meurtrier"] {
        assert_eq!(
            titles(&s.search(&q(t), NOW).await.unwrap()),
            vec!["L'Été.Meurtrier.1983.FRENCH.1080p"],
            "{t}"
        );
    }
    // Stop words are words like any other.
    assert_eq!(
        titles(&s.search(&q("the lord of the"), NOW).await.unwrap()),
        vec!["The Lord of the Rings"]
    );
    assert_eq!(
        titles(&s.search(&q("le seigneur des"), NOW).await.unwrap()),
        vec!["Le Seigneur des Anneaux"]
    );
    // Junk does not error; matches nothing that has no word.
    assert_eq!(s.search(&q("'):*&|!"), NOW).await.unwrap().total, 9);
    assert_eq!(s.search(&q("zzzzqqq"), NOW).await.unwrap().total, 0);
    // Relevance without text falls back to date.
    let mut f = q("");
    f.sort = Sort::Relevance;
    assert_eq!(
        s.search(&f, NOW).await.unwrap().hits[0].title,
        "Blender open movies"
    );
}

#[tokio::test]
async fn changes_reach_the_index_within_seconds() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(s, torrent(1, 1, NOW - 100, 1, "first release")).await;
    let ix = index_for(s).await;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let looper = tokio::spawn(search_index::index_loop(
        s.pool().clone(),
        ix.idx.clone(),
        stop_rx,
    ));

    async fn until<F: AsyncFn() -> bool>(what: &str, f: F) {
        let started = Instant::now();
        while !f().await {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{what} not indexed within 5 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!("{what}: indexed after {} ms", started.elapsed().as_millis());
    }
    put(s, torrent(2, 1, NOW - 50, 2, "second release")).await;
    until("new torrent", async || {
        s.search(&q("second"), NOW).await.unwrap().total == 1
    })
    .await;

    s.put_liveness(&live(2, Some(0), None, NOW + 100))
        .await
        .unwrap();
    until("liveness", async || {
        s.search(&q("second"), NOW)
            .await
            .unwrap()
            .hits
            .first()
            .and_then(|h| h.dead)
            == Some(true)
    })
    .await;

    s.put_trust(&TrustScore {
        pubkey: hex64(1),
        score: 40,
        factors: vec![],
        root: None,
        computed_at: NOW,
        graph_version: 1,
    })
    .await
    .unwrap();
    until("trust", async || {
        s.search(&q("first"), NOW)
            .await
            .unwrap()
            .hits
            .first()
            .map(|h| h.trust)
            == Some(40)
    })
    .await;

    let media_id: i64 = sqlx::query_scalar(
        "INSERT INTO media (kind, imdb, title, year, genres, updated_at)
         VALUES ('movie', 'tt0000001', 'First Film', 2001, '{Drama}', 0) RETURNING id",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
         VALUES ($1, $2, 'manual', 1.0, 1, 0)",
    )
    .bind(ih(1))
    .bind(media_id)
    .execute(s.pool())
    .await
    .unwrap();
    until("enrichment link", async || {
        s.search(&q("first"), NOW)
            .await
            .unwrap()
            .hits
            .first()
            .and_then(|h| h.media_title.clone())
            == Some("First Film".into())
    })
    .await;
    sqlx::query("UPDATE media SET title = 'First Film (Remastered)' WHERE id = $1")
        .bind(media_id)
        .execute(s.pool())
        .await
        .unwrap();
    until("media title", async || {
        s.search(&q("first"), NOW)
            .await
            .unwrap()
            .hits
            .first()
            .and_then(|h| h.media_title.clone())
            == Some("First Film (Remastered)".into())
    })
    .await;
    // Media tokens resolve through Postgres, then filter in the index.
    assert_eq!(
        titles(&s.search(&q("imdb:tt0000001"), NOW).await.unwrap()),
        vec!["first release"]
    );
    assert_eq!(
        titles(
            &s.search(&q(&format!("media:{media_id} first")), NOW)
                .await
                .unwrap()
        ),
        vec!["first release"]
    );
    assert_eq!(s.search(&q("imdb:tt0000002"), NOW).await.unwrap().total, 0);

    s.set_override(&hex64(1), Some(OverrideAction::Block))
        .await
        .unwrap();
    until("block", async || {
        s.search(&q(""), NOW).await.unwrap().total == 0
    })
    .await;

    let st = s.search_index_status().await.unwrap();
    assert_eq!(st.state, "ready");
    assert_eq!(st.docs, 0);
    assert!(st.last_commit.is_some() && st.bytes > 0);
    stop_tx.send(true).unwrap();
    looper.await.unwrap();
}

#[tokio::test]
async fn rebuilds_when_stale_and_resumes_a_partial_build() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for n in 1..=30 {
        put(
            s,
            torrent(n, n % 3, NOW - n as i64, n, &format!("release number {n}")),
        )
        .await;
    }
    let id = load::database_id(s.pool()).await.unwrap();
    let dir = temp_index_dir();
    {
        let idx = SearchIndex::open_async(dir.clone(), id.clone())
            .await
            .unwrap();
        search_index::build(s.pool(), &idx).await.unwrap();
        assert_eq!(idx.num_docs(), 30);
    }
    // Reopened: ready at once.
    let idx = SearchIndex::open_async(dir.clone(), id.clone())
        .await
        .unwrap();
    assert!(idx.is_ready());
    assert_eq!(idx.num_docs(), 30);
    drop(idx);
    // Another database: rebuilt from scratch.
    let idx = SearchIndex::open_async(dir.clone(), "other".into())
        .await
        .unwrap();
    assert!(!idx.is_ready());
    assert_eq!(idx.num_docs(), 0);
    drop(idx);
    // Unreadable: rebuilt.
    std::fs::write(dir.join("meta.json"), b"{ not json").unwrap();
    let idx = SearchIndex::open_async(dir.clone(), id.clone())
        .await
        .unwrap();
    assert!(!idx.is_ready());
    drop(idx);

    // A build interrupted after a committed chunk resumes after its cursor.
    let idx = SearchIndex::open_async(dir.clone(), id.clone())
        .await
        .unwrap();
    let cursor = idx.begin_build().unwrap();
    assert_eq!(cursor, "");
    let first = load::infohashes_after(s.pool(), "", 10).await.unwrap();
    let docs = load::load_docs(s.pool(), &first).await.unwrap();
    idx.add_build_chunk(docs, first.last().unwrap(), true)
        .unwrap();
    // Uncommitted work is lost with the process.
    let more = load::infohashes_after(s.pool(), first.last().unwrap(), 5)
        .await
        .unwrap();
    let docs = load::load_docs(s.pool(), &more).await.unwrap();
    idx.add_build_chunk(docs, more.last().unwrap(), false)
        .unwrap();
    drop(idx);
    let idx = SearchIndex::open_async(dir.clone(), id.clone())
        .await
        .unwrap();
    assert!(!idx.is_ready());
    assert_eq!(idx.num_docs(), 10);
    assert_eq!(idx.begin_build().unwrap(), *first.last().unwrap());
    search_index::build(s.pool(), &idx).await.unwrap();
    assert!(idx.is_ready());
    assert_eq!(idx.num_docs(), 30, "no document twice, none missing");
    drop(idx);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn facets_stats_and_api() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    let http = reqwest::Client::new();
    let get = |path: String| {
        let http = http.clone();
        let url = format!("{base}{path}");
        async move {
            let r = http.get(url).bearer_auth(TOKEN).send().await.unwrap();
            (
                r.status().as_u16(),
                r.json::<serde_json::Value>().await.unwrap(),
            )
        }
    };
    put(
        s,
        torrent(1, 1, NOW - 10, 1, "linux iso").cat(Category::Application),
    )
    .await;
    put(
        s,
        torrent(2, 2, NOW - 9, 2, "linux talk").cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(3, 2, NOW - 8, 3, "cat video").cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(4, 2, NOW - 7, 3, "cat video again").cat(Category::Video),
    )
    .await;

    // Counters behind /api/v1/stats and the crawler's publisher ranking.
    let st = s.stats().await.unwrap();
    assert_eq!((st.torrents, st.publishers, st.events), (3, 2, 4));
    assert_eq!(s.publishers(10).await.unwrap(), vec![hex64(2), hex64(1)]);

    let (code, body) = get("/api/v1/facets?field=category&min_trust=0".into()).await;
    assert_eq!(code, 503, "no index yet: {body}");
    let (_, body) = get("/api/v1/stats".into()).await;
    assert!(body["search_index"].is_null());

    let _ix = index_for(s).await;
    let f = s
        .facets(&q(""), "category", 10, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.total, 3);
    let counts: Vec<(String, u64)> = f
        .counts
        .iter()
        .map(|c| (c.value.clone(), c.count))
        .collect();
    assert_eq!(counts, vec![("video".into(), 2), ("application".into(), 1)]);
    assert!(s.facets(&q(""), "nope", 10, NOW).await.is_err());

    let (code, body) = get("/api/v1/facets?field=category&q=linux&size=5&min_trust=0".into()).await;
    assert_eq!(code, 200);
    assert_eq!(body["total"], 2);
    assert_eq!(body["counts"].as_array().unwrap().len(), 2);
    let (code, _) = get("/api/v1/facets?field=bogus".into()).await;
    assert_eq!(code, 400);
    let (code, _) = get("/api/v1/facets".into()).await;
    assert_eq!(code, 400);
    let (_, body) = get("/api/v1/stats".into()).await;
    assert_eq!(body["torrents"], 3);
    assert_eq!(body["search_index"]["state"], "ready");
    assert_eq!(body["search_index"]["docs"], 3);
    let parsed: magnetar_core::api::StatsResponse = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.search_index.unwrap().docs, 3);
}

#[tokio::test]
async fn postgres_fallback_count_is_a_lower_bound_above_the_cap() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Bulk rows straight into the tables (put_torrent one by one is slow for 1005).
    sqlx::query(
        "INSERT INTO events (id, pubkey, kind, created_at, raw, received_at)
         SELECT lpad(to_hex(n), 64, '0'), lpad('1', 64, '0'), 2003, $1 - n, '{}', 0
         FROM generate_series(1, 1005) n",
    )
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO torrents (event_id, pubkey, created_at, infohash, title, category)
         SELECT lpad(to_hex(n), 64, '0'), lpad('1', 64, '0'), $1 - n, lpad(to_hex(n), 40, '0'),
                'bulk ' || n, 'other'
         FROM generate_series(1, 1005) n",
    )
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
    let page = s.search(&q("bulk"), NOW).await.unwrap();
    assert_eq!((page.total, page.total_is_lower_bound), (1000, true));
    let mut f = q("bulk");
    f.since = Some(NOW - 5);
    let page = s.search(&f, NOW).await.unwrap();
    assert_eq!((page.total, page.total_is_lower_bound), (5, false));
    assert_eq!(
        s.stats().await.unwrap().torrents,
        1005,
        "counter trigger on bulk insert"
    );

    let _ix = index_for(s).await;
    let page = s.search(&q("bulk"), NOW).await.unwrap();
    assert_eq!(
        (page.total, page.total_is_lower_bound),
        (1005, false),
        "exact on the index"
    );
}

// --- latency, in memory (no Postgres) -------------------------------------------------------

const WORDS: &[&str] = &[
    "the",
    "lord",
    "rings",
    "french",
    "multi",
    "truefrench",
    "vostfr",
    "1080p",
    "720p",
    "2160p",
    "x264",
    "x265",
    "hevc",
    "bluray",
    "webrip",
    "web",
    "dl",
    "remux",
    "hdr",
    "dv",
    "aac",
    "ac3",
    "dts",
    "season",
    "complete",
    "integrale",
    "dune",
    "interstellar",
    "matrix",
    "ubuntu",
    "debian",
    "linux",
    "windows",
    "office",
    "album",
    "flac",
    "mp3",
    "discography",
    "anime",
    "naruto",
    "one",
    "piece",
    "episode",
    "film",
    "serie",
    "documentaire",
    "le",
    "la",
    "des",
    "les",
    "of",
    "and",
    "night",
    "day",
    "world",
    "war",
    "love",
    "story",
    "city",
    "dark",
    "blue",
];

/// Deterministic pseudo-random documents shaped like real releases.
fn synthetic_docs(n: u64) -> Vec<IndexDoc> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    (0..n)
        .map(|i| {
            let words = 3 + next() % 6;
            let title: Vec<&str> = (0..words)
                .map(|_| WORDS[(next() % WORDS.len() as u64) as usize])
                .collect();
            let title = format!("{}.{}", title.join("."), 1990 + next() % 36);
            let cat = Category::ALL[(next() % 9) as usize];
            let seeded = next() % 3;
            let trust = [0, 0, 0, 25, 25, 5][(next() % 6) as usize];
            IndexDoc {
                infohash: format!("{i:040x}"),
                event_id: format!("{i:064x}"),
                pubkey: format!("{:064x}", next() % 300),
                publishers: vec![format!("{:064x}", next() % 300)],
                publisher_trust: vec![trust],
                titles: vec![title.clone()],
                files_text: format!("{title}/{title}.mkv sample.txt"),
                description: "Encoded by somebody. Enjoy and seed!".into(),
                category: cat,
                category_path: vec![],
                created_at: NOW - (next() % 100_000_000) as i64,
                total_size: (next() % 10 != 0).then(|| next() % (50 << 30)),
                file_count: (next() % 40) as u32,
                trust: trust as u8,
                pow_bits: 0,
                source_url: None,
                seeders: (seeded > 0).then(|| (next() % 500) as u32),
                leechers: (seeded > 0).then(|| (next() % 50) as u32),
                best_peers: (seeded > 0).then(|| (next() % 500) as u32),
                expires_at: (seeded > 0).then_some(NOW + 3600),
                media: None,
            }
        })
        .collect()
}

pub fn query_set() -> Vec<(&'static str, SearchQuery)> {
    let mut out = Vec::new();
    let t = |s: &str| SearchQuery {
        text: s.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    };
    for (name, text) in [
        ("empty", ""),
        ("1 letter", "a"),
        ("2 letters", "th"),
        ("3-letter prefix", "the"),
        ("common 1080p", "1080p"),
        ("common french", "french"),
        ("rare", "documentaire"),
        ("multi-word", "lord rings 1080p"),
        ("typo d1", "ubuntoo"),
        ("typo d2", "intersteller"),
        (
            "long multi typo",
            "the lord of the rings complete integrale 1080p multi",
        ),
    ] {
        out.push((name, t(text)));
    }
    for sort in [Sort::Date, Sort::Size, Sort::Seeders] {
        for desc in [true, false] {
            let mut f = t("french");
            f.sort = sort;
            f.descending = desc;
            out.push((if desc { "sort desc" } else { "sort asc" }, f.clone()));
            f.text = String::new();
            out.push((
                if desc {
                    "browse sort desc"
                } else {
                    "browse sort asc"
                },
                f,
            ));
        }
    }
    let mut f = t("1080p");
    f.categories = vec![Category::Video, Category::Anime];
    f.min_size = Some(1 << 30);
    f.hide_dead = true;
    f.min_seeders = Some(5);
    f.since = Some(NOW - 30_000_000);
    f.min_trust = Some(20);
    out.push(("filters", f));
    let mut f = t("");
    f.offset = 180;
    f.limit = 20;
    out.push(("deep page", f));
    out
}

#[test]
fn latency_bounds_in_memory() {
    let dir = temp_index_dir();
    let n = 20_000;
    let idx = SearchIndex::open(&dir, "latency").unwrap();
    assert_eq!(idx.begin_build().unwrap(), "");
    let docs = synthetic_docs(n);
    for chunk in docs.chunks(5000) {
        idx.add_build_chunk(chunk.to_vec(), &chunk.last().unwrap().infohash, false)
            .unwrap();
    }
    idx.finish_build(0).unwrap();
    assert_eq!(idx.num_docs(), n);
    // Debug builds are ~10× slower than release; the bound only catches pathologies (a query
    // that scans postings per term, a prefix exploding over the dictionary).
    let bound = Duration::from_millis(if cfg!(debug_assertions) { 1500 } else { 150 });
    for (name, sq) in query_set() {
        let iq = index_query(&sq, &sq.text, sq.min_trust.unwrap_or(0), None, NOW);
        let mut worst = Duration::ZERO;
        for _ in 0..3 {
            let t = Instant::now();
            let r = idx.search(&iq).unwrap();
            worst = worst.max(t.elapsed());
            assert!(r.hits.len() <= iq.limit);
        }
        eprintln!(
            "{name:>20} {:>8.2} ms ({:?})",
            worst.as_secs_f64() * 1e3,
            sq.text
        );
        assert!(worst < bound, "{name}: {worst:?} over {bound:?}");
    }
    drop(idx);
    let _ = std::fs::remove_dir_all(&dir);
}
