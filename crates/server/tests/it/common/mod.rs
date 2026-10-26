//! Shared test harness: one throwaway schema per test against MAGNETAR_TEST_DATABASE_URL.
#![allow(dead_code)]

use std::time::Duration;

use magnetar_core::model::{Category, ParsedTorrent, RawEvent, TorrentFile};
use magnetar_server::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

pub const NOW: i64 = 1_790_000_000;

pub struct TestDb {
    pub store: PgStore,
    url: String,
    schema: String,
}

/// None (and a SKIPPED line) when no test database is configured.
pub async fn test_db() -> Option<TestDb> {
    let db = bare_db().await?;
    contract_media_tables(&db.store).await;
    db.store.migrate().await.expect("migrate");
    Some(db)
}

/// Neutral placeholder terms added to the built-in blocklist for tests (`zorblax` hard,
/// `wibblewob` context; no real term appears in tests).
pub const TEST_BLOCKLIST_EXTRA: &str = "zorblax\ncontext:wibblewob\n";

/// Install the built-in list + the placeholders process-wide, once. All integration tests
/// share one process (D78) and `blocklist::install` only succeeds before the first
/// `blocklist::global()`, so every database test installs it first (from `bare_db`), before
/// any ingest can fall back to the built-in list.
pub fn install_test_blocklist() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let list = magnetar_core::blocklist::Blocklist::builtin()
            .expect("built-in blocklist")
            .with_extra(TEST_BLOCKLIST_EXTRA)
            .expect("placeholder terms");
        magnetar_core::blocklist::install(list);
    });
}

/// A fresh, empty schema: nothing created, nothing migrated.
pub async fn bare_db() -> Option<TestDb> {
    install_test_blocklist();
    let Ok(url) = std::env::var("MAGNETAR_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_TEST_DATABASE_URL)");
        return None;
    };
    let schema = format!("t_{:016x}", rand::random::<u64>());
    let mut admin = PgConnection::connect(&url).await.expect("connect test db");
    // Extensions live in public so dropping a test schema never drops them.
    if let Err(e) = admin
        .execute("CREATE EXTENSION IF NOT EXISTS pg_trgm SCHEMA public")
        .await
    {
        // Concurrent creation by a parallel test: fine as long as it exists now.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')",
        )
        .fetch_one(&mut admin)
        .await
        .unwrap();
        assert!(exists, "pg_trgm: {e}");
    }
    admin
        .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .await
        .unwrap();
    let set_path = format!("SET search_path TO {schema}, public");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .after_connect(move |conn, _| {
            let sql = set_path.clone();
            Box::pin(async move {
                conn.execute(AssertSqlSafe(sql)).await?;
                Ok(())
            })
        })
        .connect_with(
            url.parse::<PgConnectOptions>()
                .unwrap()
                .application_name(&schema),
        )
        .await
        .unwrap();
    let store = PgStore::from_pool(pool);
    Some(TestDb { store, url, schema })
}

/// TEST-ONLY: the enrichment contract tables (`media`, `torrent_media`) belong to the
/// enrich/imdb migration, which this branch does not have yet. Create a mirror of the contract
/// before migrating, unless a migration in this build already creates `media`.
async fn contract_media_tables(store: &PgStore) {
    let owned = magnetar_server::store::MIGRATOR
        .iter()
        .any(|m| m.sql.as_str().contains("CREATE TABLE media "));
    if !owned {
        store
            .pool()
            .execute(include_str!("contract_media.sql"))
            .await
            .expect("contract media tables");
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let url = self.url.clone();
        let schema = self.schema.clone();
        // Async cleanup from a sync Drop: run it on a private runtime. The test's own runtime
        // is blocked here, so its pool cannot close; terminate its backends instead.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Ok(mut c) = PgConnection::connect(&url).await {
                    let _ = sqlx::query(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
                         WHERE application_name = $1 AND pid <> pg_backend_pid()",
                    )
                    .bind(&schema)
                    .execute(&mut c)
                    .await;
                    let _ = c
                        .execute(AssertSqlSafe(format!(
                            "DROP SCHEMA IF EXISTS {schema} CASCADE"
                        )))
                        .await;
                }
            });
        })
        .join()
        .unwrap();
    }
}

pub fn hex64(n: u64) -> String {
    format!("{n:064x}")
}

pub fn ih(n: u64) -> String {
    format!("{n:040x}")
}

/// Builder for hand-made torrents (no nip35 parser needed).
pub struct T {
    pub p: ParsedTorrent,
}

pub fn torrent(id: u64, pubkey: u64, created_at: i64, infohash: u64, title: &str) -> T {
    T {
        p: ParsedTorrent {
            event_id: hex64(id),
            pubkey: hex64(pubkey),
            created_at,
            infohash: ih(infohash),
            infohash_v2: None,
            title: title.to_string(),
            description: String::new(),
            files: Vec::new(),
            total_size: None,
            trackers: vec!["udp://tracker.opentrackr.org:1337/announce".into()],
            category: Category::Other,
            category_path: Vec::new(),
            hashtags: Vec::new(),
            external_ids: Vec::new(),
            source_url: None,
            published_at: None,
            pow_bits: 0,
            pow_committed: None,
        },
    }
}

impl T {
    pub fn size(mut self, s: u64) -> Self {
        self.p.total_size = Some(s);
        self
    }
    pub fn cat(mut self, c: Category) -> Self {
        self.p.category = c;
        self
    }
    pub fn desc(mut self, d: &str) -> Self {
        self.p.description = d.to_string();
        self
    }
    pub fn files(mut self, f: &[(&str, u64)]) -> Self {
        self.p.files = f
            .iter()
            .map(|(p, s)| TorrentFile {
                path: p.to_string(),
                size: Some(*s),
            })
            .collect();
        self
    }
    pub fn pow(mut self, bits: u8) -> Self {
        self.p.pow_bits = bits;
        self
    }
    pub fn bridge(mut self) -> Self {
        self.p.source_url = Some("https://nyaa.si/view/1".into());
        self
    }
    pub fn hashtag(mut self, t: &str) -> Self {
        self.p.hashtags.push(t.to_string());
        self
    }
    /// A raw event carrying the tags our filters index (x, t).
    pub fn raw(&self) -> RawEvent {
        let mut tags = vec![
            vec!["title".to_string(), self.p.title.clone()],
            vec!["x".to_string(), self.p.infohash.clone()],
        ];
        for h in &self.p.hashtags {
            tags.push(vec!["t".into(), h.clone()]);
        }
        RawEvent {
            id: self.p.event_id.clone(),
            pubkey: self.p.pubkey.clone(),
            created_at: self.p.created_at,
            kind: 2003,
            tags,
            content: self.p.description.clone(),
            sig: "0".repeat(128),
        }
    }
    pub fn build(self) -> (ParsedTorrent, RawEvent) {
        let raw = self.raw();
        (self.p, raw)
    }
}

pub async fn put(store: &PgStore, t: T) -> (ParsedTorrent, RawEvent) {
    use magnetar_core::store::Store;
    let (p, raw) = t.build();
    assert!(store.put_torrent(&p, &raw).await.unwrap());
    (p, raw)
}

/// Loads the real relay fixtures (one NIP-01 event per line).
pub fn fixture_events(name: &str) -> Vec<RawEvent> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/research/fixtures/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| RawEvent::from_json(l).unwrap())
        .collect()
}

pub const TOKEN: &str = "test-token-123";

/// In-process API server on 127.0.0.1:0. Returns the base URL.
pub async fn spawn_server(store: &PgStore) -> String {
    let state = magnetar_server::AppState::new(std::sync::Arc::new(store.clone()), TOKEN);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// A real signed event (for paths that verify signatures).
pub fn signed(
    keys: &nostr::prelude::Keys,
    kind: u16,
    tags: Vec<Vec<String>>,
    content: &str,
) -> RawEvent {
    use nostr::prelude::*;
    let tags: Vec<Tag> = tags.into_iter().map(|t| Tag::parse(t).unwrap()).collect();
    let ev = EventBuilder::new(Kind::from(kind), content)
        .tags(tags)
        .finalize(keys)
        .unwrap();
    RawEvent::from_json(&ev.as_json()).unwrap()
}

impl TestDb {
    /// DATABASE_URL for a child process confined to this test's schema.
    pub fn scoped_url(&self) -> String {
        let sep = if self.url.contains('?') { '&' } else { '?' };
        format!(
            "{}{sep}options%5Bsearch_path%5D={},public&application_name={}",
            self.url, self.schema, self.schema
        )
    }
}

/// Query mix for search latency checks (tests/search_index.rs, tests/search_bench.rs, D43):
/// browse, 1–3 letters, common and rare words, multi-word, typos, filters, every sort.
pub fn search_query_set(now: i64) -> Vec<(String, magnetar_core::query::SearchQuery)> {
    use magnetar_core::query::{SearchQuery, Sort};
    let t = |s: &str| SearchQuery {
        text: s.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    };
    let mut out: Vec<(String, SearchQuery)> = Vec::new();
    for (name, text) in [
        ("browse", ""),
        ("1 letter", "a"),
        ("1 letter", "e"),
        ("2 letters", "th"),
        ("2 letters", "le"),
        ("3-letter prefix", "the"),
        ("3-letter prefix", "one"),
        ("common", "1080p"),
        ("common", "french"),
        ("common", "multi"),
        ("common", "x265"),
        ("rare", "chernobyl"),
        ("rare", "documentaire"),
        ("multi-word", "the lord of the rings"),
        ("multi-word", "game of thrones s01"),
        ("multi-word", "lord rings 1080p"),
        ("typo", "interstelar"),
        ("typo", "matirx"),
        ("typo", "ubuntoo"),
        (
            "long",
            "the lord of the rings complete integrale 1080p multi",
        ),
    ] {
        out.push((format!("{name} {text:?}"), t(text)));
    }
    for sort in [Sort::Date, Sort::Size, Sort::Seeders] {
        for desc in [true, false] {
            let dir = if desc { "desc" } else { "asc" };
            for text in ["french", ""] {
                let mut f = t(text);
                f.sort = sort;
                f.descending = desc;
                out.push((format!("sort {sort:?} {dir} {text:?}"), f));
            }
        }
    }
    let mut f = t("1080p");
    f.categories = vec![Category::Video, Category::Anime];
    f.min_size = Some(1 << 30);
    f.hide_dead = true;
    f.min_seeders = Some(5);
    f.since = Some(now - 30_000_000);
    out.push(("filters \"1080p\"".into(), f.clone()));
    f.text = String::new();
    out.push(("filters browse".into(), f));
    let mut f = t("");
    f.min_trust = Some(20);
    out.push(("min_trust 20 browse".into(), f));
    let mut f = t("french");
    f.min_trust = Some(20);
    f.sort = Sort::Seeders;
    out.push(("min_trust 20 seeders \"french\"".into(), f));
    let mut f = t("");
    f.offset = 180;
    f.limit = 20;
    out.push(("deep page".into(), f));
    // Release/media filters (D44).
    out.push((
        "lang fr-ca browse".into(),
        SearchQuery {
            languages: vec!["fr-ca".into()],
            ..t("")
        },
    ));
    out.push((
        "fr + 1080p + h265 browse".into(),
        SearchQuery {
            languages: vec!["fr".into()],
            resolutions: vec!["1080p".into()],
            video_codecs: vec!["h265".into()],
            ..t("")
        },
    ));
    out.push((
        "fr subs \"one piece\"".into(),
        SearchQuery {
            subtitles: vec!["fr".into()],
            ..t("one piece")
        },
    ));
    out.push((
        "remux + dv + atmos browse".into(),
        SearchQuery {
            sources: vec!["remux".into()],
            hdr: vec!["dv".into()],
            audio: vec!["atmos".into(), "truehd".into()],
            ..t("")
        },
    ));
    out.push((
        "drama, rating 7, 2010-2020".into(),
        SearchQuery {
            genres: vec!["drama".into()],
            min_rating: Some(7.0),
            min_year: Some(2010),
            max_year: Some(2020),
            ..t("")
        },
    ));
    out.push((
        "sort rating \"french\"".into(),
        SearchQuery {
            sort: Sort::Rating,
            ..t("french")
        },
    ));
    out
}
