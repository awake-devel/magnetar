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
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')")
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
    store.migrate().await.expect("migrate");
    Some(TestDb { store, url, schema })
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
                        .execute(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE")))
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
