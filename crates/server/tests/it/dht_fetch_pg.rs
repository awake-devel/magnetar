//! DHT stage 2 (docs/DHT.md §2.4, §4, §7): fetch results written to Postgres — verified
//! torrents stored with their triggers (infohash row, schedule, search log, release attributes),
//! the content screen deciding first (block → hashes only; no screen → nothing named stored),
//! hybrid aliases, final statuses and backoff — and the whole pipeline from a fake DHT to a fake
//! seeding peer. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

use crate::common;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use magnetar_core::dht::krpc::Query;
use magnetar_core::dht::{Dht, EngineConfig, Mode, Via};
use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::metainfo::{
    FetchFailure, Fetched, InfoError, Outcome, PeerError, parse_info, wire,
};
use magnetar_server::dht::persist::{FetchResult, persist_batch};
use magnetar_server::dht::screen::{BlocklistScreen, Screen, UNSCREENED_RETRY_SECS, Verdict};
use magnetar_server::dht::{self, DhtOptions, store as dstore};
use sha2::Digest;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Test screen: blocks any name containing the (harmless) marker `BLOCKME`.
#[derive(Debug)]
struct MarkerScreen;

impl Screen for MarkerScreen {
    fn verdict(&self, names: &[String], _adult: bool) -> Verdict {
        if names.iter().any(|n| n.contains("BLOCKME")) {
            Verdict::Block(magnetar_core::blocklist::Tier::Hard)
        } else {
            Verdict::Allow
        }
    }
    fn rule_version(&self) -> i16 {
        7
    }
}

pub(crate) fn dict(mut e: Vec<(&str, Value)>) -> Value {
    e.sort_by(|a, b| a.0.cmp(b.0));
    Value::Dict(
        e.into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

pub(crate) fn b(s: &str) -> Value {
    Value::Bytes(s.as_bytes().to_vec())
}

/// A v1 info dict: `name` with a video file and a padding file (padding is not displayed but
/// is screened).
pub(crate) fn info(name: &str, pad_name: &str) -> Vec<u8> {
    bencode::encode(&dict(vec![
        (
            "files",
            Value::List(vec![
                dict(vec![
                    ("length", Value::Int(1_500_000_000)),
                    ("path", Value::List(vec![b(&format!("{name}.mkv"))])),
                ]),
                dict(vec![
                    ("attr", b("p")),
                    ("length", Value::Int(100)),
                    ("path", Value::List(vec![b(".pad"), b(pad_name)])),
                ]),
            ]),
        ),
        ("name", b(name)),
        ("piece length", Value::Int(262_144)),
        ("pieces", Value::Bytes(vec![1; 20])),
    ]))
}

pub(crate) fn sha1(raw: &[u8]) -> [u8; 20] {
    sha1_smol::Sha1::from(raw).digest().bytes()
}

fn fetched(raw: &[u8], sighted: [u8; 20], v2: Option<[u8; 32]>) -> Outcome {
    Outcome::Ok(Box::new(Fetched {
        sighted,
        v1: sha1(raw),
        v2,
        info: parse_info(raw).unwrap(),
        metadata_size: raw.len(),
        peers_tried: 1,
        raw: None,
    }))
}

fn ok(raw: &[u8]) -> FetchResult {
    let h = sha1(raw);
    FetchResult {
        sighted: h,
        via: Via::Sample,
        peers_seen: 3,
        result: Ok(fetched(raw, h, None)),
    }
}

async fn queue(pool: &PgPool, hashes: &[[u8; 20]]) {
    let batch: Vec<_> = hashes.iter().map(|h| (*h, Via::Sample)).collect();
    let o = dstore::triage(pool, &batch, NOW - 500, false)
        .await
        .unwrap();
    assert_eq!(o.queued.len(), hashes.len());
}

async fn seen(pool: &PgPool, h: &[u8; 20]) -> Option<(i16, i16, Option<i64>, Option<String>)> {
    sqlx::query_as(
        "SELECT status, attempts, next_attempt_at, last_error FROM dht_seen WHERE infohash = $1",
    )
    .bind(h.as_slice())
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn count(pool: &PgPool, sql: &'static str, h: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(h)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn verified_torrents_are_stored_and_searchable() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let raw = info("Some.Movie.2023.1080p.BluRay.x264-GRP", "0");
    let h = sha1(&raw);
    queue(pool, &[h]).await;
    let screen = MarkerScreen;
    let o = persist_batch(pool, &[ok(&raw)], Some(&screen), false, NOW)
        .await
        .unwrap();
    assert_eq!(o.stored, 1);
    let hx = hex::encode(h);
    let row: (
        String,
        i32,
        i64,
        String,
        i16,
        i64,
        i64,
        String,
        i32,
        i32,
        String,
    ) = sqlx::query_as(
        "SELECT title, file_count, total_size, category, classifier_version, first_seen,
                    fetched_at, discovered_via, peers_seen, metadata_size, files_text
             FROM dht_torrents WHERE infohash = $1",
    )
    .bind(&hx)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(row.0, "Some.Movie.2023.1080p.BluRay.x264-GRP");
    assert_eq!(row.1, 1, "padding file dropped");
    assert_eq!(row.2, 1_500_000_000);
    assert_eq!(
        (row.3.as_str(), row.4),
        ("video", magnetar_core::classify::CLASSIFIER_VERSION),
        "classified (stage 3)"
    );
    assert_eq!((row.5, row.6), (NOW - 500, NOW), "first_seen from dht_seen");
    assert_eq!((row.7.as_str(), row.8), ("sample", 3));
    assert_eq!(row.9 as usize, raw.len());
    assert_eq!(row.10, "Some.Movie.2023.1080p.BluRay.x264-GRP.mkv");
    // Triggers: infohash row with dht = true, seed-check schedule, search log; SQL text search.
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM torrent_infohashes WHERE infohash = $1 AND dht",
            &hx
        )
        .await,
        1
    );
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM liveness_schedule WHERE infohash = $1",
            &hx
        )
        .await,
        1
    );
    // (the attributes insert logs the hash too)
    assert!(
        count(
            pool,
            "SELECT count(*) FROM search_changes WHERE infohash = $1",
            &hx
        )
        .await
            >= 1
    );
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1
               AND search_tsv @@ plainto_tsquery('simple', 'movie bluray')",
            &hx
        )
        .await,
        1
    );
    // Release attributes parsed from the DHT name (D44).
    let attrs: serde_json::Value =
        sqlx::query_scalar("SELECT attrs FROM torrent_attrs WHERE infohash = $1")
            .bind(&hx)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(attrs["resolution"], "1080p", "{attrs}");
    assert!(
        seen(pool, &h).await.is_none(),
        "stored hashes leave dht_seen"
    );

    // The same hash again (another batch): not stored twice.
    let o = persist_batch(pool, &[ok(&raw)], Some(&screen), false, NOW)
        .await
        .unwrap();
    assert_eq!((o.stored, o.not_new), (0, 1));
}

#[tokio::test]
async fn the_screen_runs_first_and_fails_closed() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    // The marker hides in a padding file's path: not displayed, still screened.
    let bad = info("Harmless.Name", "BLOCKME");
    let good = info("Fine.Name", "1");
    let (hb, hg) = (sha1(&bad), sha1(&good));
    queue(pool, &[hb, hg]).await;

    // No screen: nothing named stored, the hashes go back to the queue for later.
    let o = persist_batch(pool, &[ok(&bad), ok(&good)], None, false, NOW)
        .await
        .unwrap();
    assert_eq!((o.unscreened, o.stored), (2, 0));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM dht_torrents")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let (status, attempts, next, err) = seen(pool, &hg).await.unwrap();
    assert_eq!((status, attempts), (0, 0));
    assert_eq!(next, Some(NOW + UNSCREENED_RETRY_SECS));
    assert_eq!(err.as_deref(), Some("unscreened"));

    // With the screen: blocked → only the hash, with the rule version; the other is stored.
    let o = persist_batch(
        pool,
        &[ok(&bad), ok(&good)],
        Some(&MarkerScreen),
        false,
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.blocked, o.stored), (1, 1));
    let (rv, origin): (i16, String) =
        sqlx::query_as("SELECT rule_version, origin FROM blocked_infohashes WHERE infohash = $1")
            .bind(hex::encode(hb))
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((rv, origin.as_str()), (7, "dht"));
    assert!(seen(pool, &hb).await.is_none());
    // No trace of the blocked name anywhere it could have been written.
    for sql in [
        "SELECT count(*) FROM dht_torrents WHERE title LIKE '%' || $1 || '%' OR files_text LIKE '%' || $1 || '%' OR files::text LIKE '%' || $1 || '%'",
        "SELECT count(*) FROM torrent_attrs WHERE attrs::text LIKE '%' || $1 || '%'",
        "SELECT count(*) FROM dht_seen WHERE coalesce(last_error, '') LIKE '%' || $1 || '%'",
    ] {
        assert_eq!(count(pool, sql, "Harmless").await, 0, "{sql}");
    }
    // Triage now drops the blocked hash.
    let t = dstore::triage(pool, &[(hb, Via::Sample)], NOW + 10, false)
        .await
        .unwrap();
    assert_eq!((t.blocked, t.queued.len()), (1, 0));
}

#[tokio::test]
async fn hybrid_sighted_by_v2_is_stored_under_v1_with_an_alias() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let tree = dict(vec![(
        "a.bin",
        dict(vec![(
            "",
            dict(vec![
                ("length", Value::Int(5000)),
                ("pieces root", Value::Bytes(vec![1; 32])),
            ]),
        )]),
    )]);
    let mk = |name: &str| {
        bencode::encode(&dict(vec![
            ("file tree", tree.clone()),
            ("length", Value::Int(5000)),
            ("meta version", Value::Int(2)),
            ("name", b(name)),
            ("piece length", Value::Int(16384)),
            ("pieces", Value::Bytes(vec![2; 20])),
        ]))
    };
    let raw = mk("Hybrid.One");
    let v2: [u8; 32] = sha2::Sha256::digest(&raw).into();
    let t: [u8; 20] = v2[..20].try_into().unwrap();
    queue(pool, &[t]).await;
    let r = FetchResult {
        sighted: t,
        via: Via::Sample,
        peers_seen: 1,
        result: Ok(fetched(&raw, t, Some(v2))),
    };
    let o = persist_batch(pool, &[r], Some(&MarkerScreen), false, NOW)
        .await
        .unwrap();
    assert_eq!((o.stored, o.alias), (1, 1));
    let v2_hex: Option<String> =
        sqlx::query_scalar("SELECT infohash_v2 FROM dht_torrents WHERE infohash = $1")
            .bind(hex::encode(sha1(&raw)))
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(v2_hex, Some(hex::encode(v2)));
    let (status, _, next, _) = seen(pool, &t).await.unwrap();
    assert_eq!(
        (status, next),
        (8, None),
        "sighted truncated hash kept as alias"
    );

    // A hybrid whose SHA-1 is already known (Nostr) is not stored again; alias all the same.
    let raw2 = mk("Hybrid.Two");
    let v2b: [u8; 32] = sha2::Sha256::digest(&raw2).into();
    let t2: [u8; 20] = v2b[..20].try_into().unwrap();
    sqlx::query("INSERT INTO torrent_infohashes (infohash, events) VALUES ($1, 1)")
        .bind(hex::encode(sha1(&raw2)))
        .execute(pool)
        .await
        .unwrap();
    queue(pool, &[t2]).await;
    let r = FetchResult {
        sighted: t2,
        via: Via::Sample,
        peers_seen: 1,
        result: Ok(fetched(&raw2, t2, Some(v2b))),
    };
    let o = persist_batch(pool, &[r], Some(&MarkerScreen), false, NOW)
        .await
        .unwrap();
    assert_eq!((o.stored, o.not_new, o.alias), (0, 1, 1));
    assert_eq!(seen(pool, &t2).await.unwrap().0, 8);
}

#[tokio::test]
async fn final_statuses_and_backoff() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let hs: Vec<[u8; 20]> = (1..=5u8).map(|i| [i; 20]).collect();
    queue(pool, &hs).await;
    let fail = |error, oversize| FetchFailure {
        error,
        oversize,
        peers_tried: 2,
        skipped: 0,
    };
    let items = vec![
        FetchResult {
            sighted: hs[0],
            via: Via::Sample,
            peers_seen: 1,
            result: Ok(Outcome::Private),
        },
        FetchResult {
            sighted: hs[1],
            via: Via::Sample,
            peers_seen: 1,
            result: Ok(Outcome::V2Only),
        },
        FetchResult {
            sighted: hs[2],
            via: Via::Sample,
            peers_seen: 1,
            result: Ok(Outcome::Invalid(InfoError::DuplicateKeys)),
        },
        FetchResult {
            sighted: hs[3],
            via: Via::Sample,
            peers_seen: 1,
            result: Err(fail(PeerError::Oversize, true)),
        },
        FetchResult {
            sighted: hs[4],
            via: Via::Sample,
            peers_seen: 1,
            result: Err(fail(PeerError::HashMismatch, false)),
        },
    ];
    let o = persist_batch(pool, &items, Some(&MarkerScreen), false, NOW)
        .await
        .unwrap();
    assert_eq!(
        (o.private, o.v2_only, o.invalid, o.oversize, o.failed),
        (1, 1, 1, 1, 1)
    );
    let want = [
        (4, None, "private"),
        (5, None, "v2_only"),
        (3, None, "duplicate_keys"),
        (6, None, "oversize"),
        (2, Some(NOW + dstore::BACKOFF_SECS[0]), "hash_mismatch"),
    ];
    for (h, (st, next, code)) in hs.iter().zip(want) {
        let (status, attempts, n, err) = seen(pool, h).await.unwrap();
        assert_eq!(
            (status, attempts, n, err.as_deref()),
            (st, 1, next, Some(code))
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Whole pipeline: fake DHT → peer collection → BEP 9 fetch from a fake seeder → Postgres.

/// A seeder on `ip` that serves every dict in `dicts` (by infohash) over BEP 9.
pub(crate) async fn seeder(ip: Ipv4Addr, dicts: HashMap<[u8; 20], Vec<u8>>) -> u16 {
    let l = TcpListener::bind((ip, 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let dicts = Arc::new(dicts);
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(seed_one(s, dicts.clone()));
        }
    });
    port
}

async fn seed_one(mut s: TcpStream, dicts: Arc<HashMap<[u8; 20], Vec<u8>>>) -> Option<()> {
    let mut hs = [0u8; 68];
    s.read_exact(&mut hs).await.ok()?;
    let h: [u8; 20] = hs[28..48].try_into().ok()?;
    let raw = dicts.get(&h)?;
    s.write_all(&wire::handshake(&h, &[9; 20])).await.ok()?;
    let ext = dict(vec![
        ("m", dict(vec![("ut_metadata", Value::Int(2))])),
        ("metadata_size", Value::Int(raw.len() as i64)),
    ]);
    s.write_all(&wire::extended(0, &bencode::encode(&ext)))
        .await
        .ok()?;
    loop {
        let len = s.read_u32().await.ok()? as usize;
        if len == 0 {
            continue;
        }
        let mut m = vec![0u8; len];
        s.read_exact(&mut m).await.ok()?;
        if m[0] != 20 || m.get(1) != Some(&2) {
            continue;
        }
        let (req, _) = bencode::decode_prefix(&m[2..]).ok()?;
        let piece = req.get(b"piece").and_then(Value::as_int)? as usize;
        let start = piece * wire::BLOCK;
        let end = (start + wire::BLOCK).min(raw.len());
        let mut out = bencode::encode(&dict(vec![
            ("msg_type", Value::Int(1)),
            ("piece", Value::Int(piece as i64)),
            ("total_size", Value::Int(raw.len() as i64)),
        ]));
        out.extend_from_slice(raw.get(start..end)?);
        s.write_all(&wire::extended(1, &out)).await.ok()?;
    }
}

pub(crate) fn fake_cfg(ip: Ipv4Addr, bootstrap: Vec<SocketAddrV4>) -> EngineConfig {
    let mut c = EngineConfig::new(Mode::Aggressive);
    c.bind = ip;
    c.ports = vec![0];
    c.qps = 400;
    c.pool_cap = 10_000;
    c.bootstrap = bootstrap;
    c.allow_loopback = true;
    c.query_timeout = Duration::from_millis(500);
    c.per_ip_qps = 100.0;
    c.response_per_ip = 1000.0;
    c.response_rate = 10_000.0;
    c.vote_responders = 3;
    c.vote_prefixes = 1;
    c.sample = false;
    c
}

pub(crate) async fn announce(from: &Dht, to: SocketAddrV4, hash: [u8; 20], port: u16) {
    // The fake network answers well within the query timeout, but the shared test process
    // (D78) can be busy: a lost query is retried.
    let mut r = None;
    for _ in 0..5 {
        if let Ok(x) = from.query(0, to, Query::GetPeers { info_hash: hash }).await {
            r = Some(x);
            break;
        }
    }
    let token = r.expect("get_peers").token.expect("token");
    let mut announced = false;
    for _ in 0..5 {
        let q = Query::AnnouncePeer {
            info_hash: hash,
            port,
            implied_port: false,
            token: token.clone(),
        };
        if from.query(0, to, q).await.is_ok() {
            announced = true;
            break;
        }
    }
    assert!(announced, "announce");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_fetches_and_stores_from_a_fake_seeder() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..6u8 {
        let ip = Ipv4Addr::new(127, 0, 9, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    // 20 torrents seeded by one peer; one carries the screen's marker.
    let client_ip = Ipv4Addr::new(127, 0, 10, 1);
    let mut dicts = HashMap::new();
    for k in 0..20 {
        let name = if k == 7 {
            format!("Torrent.{k}.BLOCKME")
        } else {
            format!("Torrent.{k}.2024.720p")
        };
        let raw = info(&name, "0");
        dicts.insert(sha1(&raw), raw);
    }
    let blocked_hash = *dicts
        .iter()
        .find(|(_, r)| parse_info(r).unwrap().name.contains("BLOCKME"))
        .unwrap()
        .0;
    let hashes: Vec<[u8; 20]> = dicts.keys().copied().collect();
    let port = seeder(client_ip, dicts).await;
    let (client, _crx) = Dht::start(fake_cfg(client_ip, vec![net[0].1]))
        .await
        .unwrap();
    for (k, h) in hashes.iter().enumerate() {
        announce(&client, net[k % net.len()].1, *h, port).await;
    }

    let crawler_ip = Ipv4Addr::new(127, 0, 11, 1);
    let opts = DhtOptions {
        mode: Mode::Aggressive,
        ports: vec![0],
        qps: 400,
        pool_cap: 10_000,
        fetch_known: false,
        peer_lookups: 8,
        lookup_timeout: Duration::from_millis(1500),
        peers_per_hash: 12,
        sample_priority: 0.0,
        dry_run: false,
        bloom_bits: 1 << 20,
        queue_cap: 1000,
        fetch_concurrency: 8,
        metadata_max: 1 << 20,
        tcp_rate: 1000,
        tcp_max: 350,
        probe: Vec::new(),
        screen: Some(Arc::new(MarkerScreen)),
        upnp: None,
        service: None,
        geo: None,
        pacer: None,
        health: None,
    };
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let rep = dht::run(
        Some(pool.clone()),
        opts,
        stop,
        Some(Duration::from_secs(8)),
        move |c: &mut EngineConfig| {
            c.bind = crawler_ip;
            c.bootstrap = vec![net[0].1];
            c.allow_loopback = true;
            c.query_timeout = Duration::from_millis(500);
            c.per_ip_qps = 100.0;
            c.response_per_ip = 1000.0;
            c.response_rate = 10_000.0;
            c.vote_responders = 3;
            c.vote_prefixes = 1;
        },
    )
    .await
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&rep).unwrap());
    assert_eq!(rep.persist_errors, 0);
    assert!(rep.fetch.ok >= 18, "{:?}", rep.fetch);
    assert!(rep.persist.stored >= 17, "{:?}", rep.persist);
    assert!(rep.bandwidth.tcp_in > 0 && rep.bandwidth.udp_in > 0);
    let stored: Vec<(String, String)> =
        sqlx::query_as("SELECT infohash, discovered_via FROM dht_torrents")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len() as u64, rep.persist.stored);
    assert!(
        !stored.iter().any(|(h, _)| *h == hex::encode(blocked_hash)),
        "blocked torrent stored"
    );
    assert_eq!(rep.persist.blocked, 1, "{:?}", rep.persist);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM blocked_infohashes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    client.shutdown().await;
}

/// Log capture for the whole test process (TRACE, every target).
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Stage 3 (D53, D68, D69): the real `core::blocklist` screen with neutral placeholder terms
/// (`zorblax` hard, `wibblewob` context), the classifier deciding the context tier, through
/// the whole pipeline at TRACE: blocked torrents leave only hashes, stored ones carry their
/// category, and no torrent name or path ever appears in the logs or the report.
///
/// The capture covers every thread of this test's own runtime (workers and blocking pool)
/// rather than the whole process: all integration tests share one process (D78), and a
/// process-wide subscriber would also record the other tests' fixture names.
#[test]
fn pipeline_blocklist_classifies_and_never_logs_names() {
    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(cap.clone())
        .with_ansi(false)
        .finish();
    // tracing-core takes a fast path while one dispatcher exists: callsite interest then
    // comes from the registering thread's default, which is none for a scoped dispatcher (and
    // for the other tests' threads), and every callsite would be disabled. A second live
    // dispatcher makes interest the union over all of them, checked per event.
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let dispatch = tracing::Dispatch::new(sub);
    let on_start = dispatch.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .on_thread_start(move || {
            // Kept for the thread's lifetime: the runtime's threads end with the runtime.
            std::mem::forget(tracing::dispatcher::set_default(&on_start));
        })
        .build()
        .unwrap();
    let _main = tracing::dispatcher::set_default(&dispatch);
    rt.block_on(pipeline_blocklist_run(cap));
}

async fn pipeline_blocklist_run(cap: Capture) {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..6u8 {
        let ip = Ipv4Addr::new(127, 0, 19, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    // (name, padding path, expected: Some(category) stored, None blocked)
    let fixtures: Vec<(String, &str, Option<&str>)> = vec![
        ("Plain.Show.2024.720p".into(), "zorblax", None), // hard, in a padding path
        ("Studio.MILF.Scene.Wibblewob.1080p".into(), "0", None), // context, adult
        ("Studio.XXX.Clip.12.yo".into(), "0", None),      // age rule, adult
        (
            "Wibblewob.Documentary.2020.1080p".into(),
            "0",
            Some("video"),
        ), // context, not adult
        ("Studio.XXX.Scene.2024.1080p".into(), "0", Some("porn")),
        ("[Grp] Some Show - 05 (1080p)".into(), "0", Some("anime")),
    ];
    let mut dicts = HashMap::new();
    let mut expect = HashMap::new();
    for (name, pad, want) in &fixtures {
        let raw = info(name, pad);
        let h = sha1(&raw);
        expect.insert(hex::encode(h), *want);
        dicts.insert(h, raw);
    }
    for k in 0..6 {
        let raw = info(&format!("Filler.Torrent.{k}.2024.720p"), "0");
        let h = sha1(&raw);
        expect.insert(hex::encode(h), Some("video"));
        dicts.insert(h, raw);
    }
    let hashes: Vec<[u8; 20]> = dicts.keys().copied().collect();
    let client_ip = Ipv4Addr::new(127, 0, 20, 1);
    let port = seeder(client_ip, dicts).await;
    let (client, _crx) = Dht::start(fake_cfg(client_ip, vec![net[0].1]))
        .await
        .unwrap();
    for (k, h) in hashes.iter().enumerate() {
        announce(&client, net[k % net.len()].1, *h, port).await;
    }
    let list = magnetar_core::blocklist::Blocklist::builtin()
        .unwrap()
        .with_extra("zorblax\ncontext:wibblewob\n")
        .unwrap();
    let opts = DhtOptions {
        mode: Mode::Aggressive,
        ports: vec![0],
        qps: 400,
        pool_cap: 10_000,
        fetch_known: false,
        peer_lookups: 8,
        lookup_timeout: Duration::from_millis(1500),
        peers_per_hash: 12,
        sample_priority: 0.0,
        dry_run: false,
        bloom_bits: 1 << 20,
        queue_cap: 1000,
        fetch_concurrency: 8,
        metadata_max: 1 << 20,
        tcp_rate: 1000,
        tcp_max: 350,
        probe: Vec::new(),
        screen: Some(Arc::new(BlocklistScreen(Arc::new(list)))),
        upnp: None,
        service: None,
        geo: None,
        pacer: None,
        health: None,
    };
    let crawler_ip = Ipv4Addr::new(127, 0, 21, 1);
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let rep = dht::run(
        Some(pool.clone()),
        opts,
        stop,
        Some(Duration::from_secs(8)),
        move |c: &mut EngineConfig| {
            c.bind = crawler_ip;
            c.bootstrap = vec![net[0].1];
            c.allow_loopback = true;
            c.query_timeout = Duration::from_millis(500);
            c.per_ip_qps = 100.0;
            c.response_per_ip = 1000.0;
            c.response_rate = 10_000.0;
            c.vote_responders = 3;
            c.vote_prefixes = 1;
        },
    )
    .await
    .unwrap();
    client.shutdown().await;
    let report = serde_json::to_string(&rep).unwrap();
    assert_eq!(rep.persist_errors, 0);
    assert_eq!(rep.persist.blocked, 3, "{report}");
    for tier in ["hard", "context", "age"] {
        assert_eq!(rep.persist.blocked_by_tier.get(tier), Some(&1), "{report}");
    }
    let stored: HashMap<String, (String, i16)> = sqlx::query_as::<_, (String, String, i16)>(
        "SELECT infohash, category, classifier_version FROM dht_torrents",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(h, c, v)| (h, (c, v)))
    .collect();
    let blocked: Vec<String> = sqlx::query_scalar("SELECT infohash FROM blocked_infohashes")
        .fetch_all(&pool)
        .await
        .unwrap();
    for (h, want) in &expect {
        match want {
            Some(cat) => {
                let got = stored.get(h).expect("stored");
                assert_eq!(
                    (got.0.as_str(), got.1),
                    (*cat, magnetar_core::classify::CLASSIFIER_VERSION)
                );
            }
            None => {
                assert!(!stored.contains_key(h));
                assert!(blocked.contains(h));
            }
        }
    }
    assert_eq!(rep.persist.categories.get("porn"), Some(&1), "{report}");
    // Nothing named in the logs (this test's runtime, TRACE) or the report.
    let logs = String::from_utf8_lossy(&cap.0.lock().unwrap()).to_lowercase();
    assert!(
        logs.len() > 1000,
        "the capture saw the run ({} bytes)",
        logs.len()
    );
    for needle in [
        "zorblax",
        "wibblewob",
        "studio",
        "plain.show",
        "filler",
        "some show",
        "documentary",
        ".mkv",
    ] {
        assert!(!logs.contains(needle), "{needle:?} logged");
        assert!(
            !report.to_lowercase().contains(needle),
            "{needle:?} in the report"
        );
    }
}
