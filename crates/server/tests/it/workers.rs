//! Crawl workers (D96–D100, docs/WORKERS.md) against a scratch schema: auth, seed-check
//! leases, verification of uploads, server-side classification and blocklist, hash-only block
//! reports, version skew, and two in-process workers sharing seed checks and a fake DHT.
//! Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED). Blocklist terms are the test placeholders
//! (`zorblax` hard, `wibblewob` context; common::TEST_BLOCKLIST_EXTRA).

use crate::common;
use crate::dht_fetch_pg::{announce, fake_cfg, info, seeder, sha1};

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use common::*;
use magnetar_core::dht::{Dht, EngineConfig, Mode};
use magnetar_core::liveness::LivenessConfig;
use magnetar_server::dht::{DhtOptions, screen};
use magnetar_server::fleet::proto::*;
use magnetar_server::fleet::{Fleet, FleetConfig, parse_tokens};
use magnetar_server::worker::client::Client;
use magnetar_server::worker::governor::Limits;
use magnetar_server::worker::{self, WorkerOptions};
use magnetar_server::{AppState, PgStore};
use serde_json::{Value as J, json};
use tokio::net::UdpSocket;

const W1: &str = "w1-token-aaaaaaaaaaaaaaaaaaaaaaaa";
const W2: &str = "w2-token-bbbbbbbbbbbbbbbbbbbbbbbb";

type Bodies = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// The API router with workers `w1`/`w2`, recording every worker DHT request body.
async fn spawn(store: &PgStore, lease_secs: i64) -> (String, Bodies) {
    let fleet = Fleet::new(
        FleetConfig {
            tokens: parse_tokens(&format!("w1:{W1},w2:{W2}"), Some(TOKEN)).unwrap(),
            lease_secs,
            max_lease: 100,
            metadata_max: 1 << 20,
            fetch_known: false,
        },
        screen::global_screen(),
    );
    let state = AppState::new(Arc::new(store.clone()), TOKEN).with_fleet(Arc::new(fleet));
    let bodies: Bodies = Arc::default();
    let rec = bodies.clone();
    let app = magnetar_server::router(state).layer(middleware::from_fn(
        move |req: Request, next: Next| {
            let rec = rec.clone();
            async move {
                let path = req.uri().path().to_string();
                let (parts, body) = req.into_parts();
                let bytes = axum::body::to_bytes(body, 64 << 20).await.unwrap();
                if path.starts_with("/api/v1/worker/dht/") {
                    rec.lock().unwrap().push((path, bytes.to_vec()));
                }
                let r: Response = next
                    .run(Request::from_parts(parts, Body::from(bytes)))
                    .await;
                r
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), bodies)
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post(base: &str, path: &str, token: &str, api: Option<&str>, body: J) -> (u16, J) {
    let mut rb = http()
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body);
    if let Some(v) = api {
        rb = rb.header(API_HEADER, v);
    }
    let r = rb.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(J::Null))
}

async fn get(base: &str, path: &str, token: &str) -> u16 {
    http()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// BEP 15 fake answering every hash with 5 seeders.
async fn fake_tracker() -> (String, Arc<AtomicUsize>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let url = format!("udp://{}/announce", sock.local_addr().unwrap());
    let n = Arc::new(AtomicUsize::new(0));
    let c = n.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let (len, src) = sock.recv_from(&mut buf).await.unwrap();
            let req = &buf[..len];
            let action = u32::from_be_bytes(req[8..12].try_into().unwrap());
            let mut resp = Vec::new();
            resp.extend_from_slice(&action.to_be_bytes());
            resp.extend_from_slice(&req[12..16]);
            if action == 0 {
                resp.extend_from_slice(&7u64.to_be_bytes());
            } else {
                for _ in req[16..].chunks(20) {
                    c.fetch_add(1, Ordering::SeqCst);
                    resp.extend_from_slice(&5u32.to_be_bytes());
                    resp.extend_from_slice(&0u32.to_be_bytes());
                    resp.extend_from_slice(&1u32.to_be_bytes());
                }
            }
            sock.send_to(&resp, src).await.unwrap();
        }
    });
    (url, n)
}

async fn put_torrents(s: &PgStore, n: u64, tracker: &str) {
    for i in 1..=n {
        let mut t = torrent(
            i,
            1,
            NOW - 3600 - i as i64,
            0x1000 + i,
            &format!("torrent {i}"),
        );
        t.p.trackers = vec![tracker.to_string()];
        put(s, t).await;
    }
}

#[tokio::test]
async fn tokens_are_separated_and_versions_checked() {
    let Some(db) = test_db().await else { return };
    let (base, _) = spawn(&db.store, 900).await;
    let v = Some("1");
    // The phone token cannot push anything.
    for path in [
        HELLO,
        LEASE,
        LIVENESS_RESULTS,
        TRIAGE,
        DHT_RESULTS,
        HEARTBEAT,
    ] {
        let (s, j) = post(&base, path, TOKEN, v, json!({})).await;
        assert_eq!(
            (s, j["error"].as_str()),
            (401, Some("unauthorized")),
            "{path}"
        );
    }
    // A worker token reads and administers nothing.
    for path in [
        "/api/v1/stats",
        "/api/v1/search?q=x",
        "/api/v1/settings/trust",
        "/api/v1/relays",
        "/api/v1/saved-searches",
        "/api/v1/downloads",
    ] {
        assert_eq!(get(&base, path, W1).await, 401, "{path}");
    }
    let r = http()
        .put(format!("{base}/api/v1/settings/trust"))
        .bearer_auth(W1)
        .json(&json!({"root": null, "threshold": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let (s, _) = post(&base, "/api/v1/events/batch", W1, v, json!({"events": []})).await;
    assert_eq!(s, 401);
    // Unknown token, missing or other worker API.
    assert_eq!(post(&base, HELLO, "nope", v, json!({})).await.0, 401);
    let (s, j) = post(&base, HELLO, W1, None, json!({})).await;
    assert_eq!((s, j["error"].as_str()), (409, Some("worker_api_mismatch")));
    let (s, j) = post(&base, HELLO, W1, Some("2"), json!({})).await;
    assert_eq!(s, 409);
    let msg = j["message"].as_str().unwrap();
    assert!(
        msg.contains("worker API 1") && msg.contains("speaks 2"),
        "{msg}"
    );
    // The right token and version.
    let (s, j) = post(&base, HELLO, W2, v, json!({"version": "x"})).await;
    assert_eq!((s, j["name"].as_str()), (200, Some("w2")));
    assert_eq!(j["worker_api"], 1);
    // /health lists the feature; stats (API token) list both workers.
    let h: J = http()
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        h["features"]
            .as_array()
            .unwrap()
            .contains(&json!("workers"))
    );
    let st: J = http()
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w = st["workers"].as_array().unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(
        (w[0]["name"].as_str(), w[0]["connected"].as_bool()),
        (Some("w1"), Some(false))
    );
    assert_eq!(
        (w[1]["name"].as_str(), w[1]["connected"].as_bool()),
        (Some("w2"), Some(true))
    );

    // A server without worker tokens says so.
    let state = AppState::new(Arc::new(db.store.clone()), TOKEN);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, magnetar_server::router(state))
            .await
            .unwrap()
    });
    let (s, j) = post(&format!("http://{a}"), HELLO, W1, v, json!({})).await;
    assert_eq!((s, j["error"].as_str()), (403, Some("workers_disabled")));
}

#[tokio::test]
async fn worker_refuses_an_incompatible_or_old_server() {
    use axum::routing::post as p;
    let app = axum::Router::new()
        .route(
            "/api/v1/worker/hello",
            p(|| async {
                (
                    axum::http::StatusCode::CONFLICT,
                    axum::Json(json!({"error": "worker_api_mismatch", "message": "server speaks worker API 9"})),
                )
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let old = axum::Router::new();
    let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a2 = l2.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l2, old).await.unwrap() });
    for (url, want) in [
        (
            format!("http://{a}"),
            "incompatible server: server speaks worker API 9",
        ),
        (format!("http://{a2}"), "has no worker API"),
    ] {
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let err = worker::run(
            opts(&url, W1, None, Some(LivenessConfig::default())),
            stop,
            None,
            |_| {},
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains(want), "{err}");
    }
}

fn state_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("mg-worker-{:016x}", rand::random::<u64>()))
}

fn opts(
    base: &str,
    token: &str,
    dht: Option<DhtOptions>,
    liveness: Option<LivenessConfig>,
) -> WorkerOptions {
    WorkerOptions {
        server: base.into(),
        token: token.into(),
        state_dir: state_dir(),
        liveness,
        liveness_batch: 15,
        dht,
        limits: Limits {
            min_free_mb: 0,
            rss_max_mb: 0,
        },
        upload_buffer: 4 << 20,
        heartbeat_every: Duration::from_secs(1),
        geo: None,
        pacer: None,
        probe: Vec::new(),
    }
}

async fn schedule(s: &PgStore, ih: &str) -> (i64, Option<i64>) {
    sqlx::query_as("SELECT due_at, checked_at FROM liveness_schedule WHERE infohash = $1")
        .bind(ih)
        .fetch_one(s.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn leases_are_exclusive_expire_and_come_back() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put_torrents(s, 10, "udp://127.0.0.1:1/announce").await;
    let (base, _) = spawn(s, 2).await;
    let c1 = Client::new(&base, W1).unwrap();
    let c2 = Client::new(&base, W2).unwrap();
    let a: Lease = c1.post(LEASE, &LeaseRequest { max: 6 }).await.unwrap();
    let b: Lease = c2.post(LEASE, &LeaseRequest { max: 10 }).await.unwrap();
    assert_eq!((a.items.len(), b.items.len()), (6, 4));
    let ia: Vec<String> = a.items.iter().map(|i| i.infohash.clone()).collect();
    let ib: Vec<String> = b.items.iter().map(|i| i.infohash.clone()).collect();
    assert!(
        ia.iter().all(|h| !ib.contains(h)),
        "never two workers on one item"
    );
    assert!(a.items[0].trackers == ["udp://127.0.0.1:1/announce"]);
    // Nothing is left for anyone, the server's own checker included.
    let none: Lease = c2.post(LEASE, &LeaseRequest { max: 10 }).await.unwrap();
    assert!(none.items.is_empty());
    let now = magnetar_core::now();
    assert!(
        magnetar_server::liveness::claim(s.pool(), now, 10, "", 60)
            .await
            .unwrap()
            .is_empty()
    );

    // w2 answers its lease; a result for an infohash it does not hold is ignored.
    let res = |ihs: &[String]| LivenessResults {
        results: ihs
            .iter()
            .map(|h| magnetar_core::model::Liveness {
                infohash: h.clone(),
                seeders: Some(3),
                leechers: Some(1),
                completed: None,
                dht_peers_est: None,
                source: magnetar_core::model::LivenessSource::Udp,
                trackers_ok: 1,
                trackers_failed: 0,
                checked_at: magnetar_core::now(),
                expires_at: 0,
            })
            .collect(),
    };
    let mut mixed = ib.clone();
    mixed.push(ia[0].clone());
    let ack: Ack = c2.post(LIVENESS_RESULTS, &res(&mixed)).await.unwrap();
    assert_eq!((ack.accepted, ack.ignored), (4, 1));
    for h in &ib {
        let (due, checked) = schedule(s, h).await;
        assert!(
            checked.is_some() && due > now + 3600,
            "scheduled as its own check"
        );
        let l = magnetar_core::store::Store::get_liveness(s, h)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(l.seeders, Some(3));
    }
    assert!(schedule(s, &ia[0]).await.1.is_none(), "w1's item untouched");
    // w1 dies: after 2 s its lease is due again and goes to w2.
    tokio::time::sleep(Duration::from_millis(3100)).await;
    let again: Lease = c2.post(LEASE, &LeaseRequest { max: 10 }).await.unwrap();
    let mut got: Vec<String> = again.items.iter().map(|i| i.infohash.clone()).collect();
    got.sort();
    let mut want = ia.clone();
    want.sort();
    assert_eq!(got, want, "expired leases are re-issued");
    // w1's late answer for the re-leased items is ignored; w2's is taken.
    let ack: Ack = c1.post(LIVENESS_RESULTS, &res(&ia)).await.unwrap();
    assert_eq!((ack.accepted, ack.ignored), (0, 6));
    // Releasing gives items back at once.
    let ack: Ack = c2
        .post(
            RELEASE,
            &Release {
                infohashes: ia[..2].to_vec(),
            },
        )
        .await
        .unwrap();
    assert_eq!(ack.accepted, 2);
    let back: Lease = c1.post(LEASE, &LeaseRequest { max: 10 }).await.unwrap();
    assert_eq!(back.items.len(), 2);
    let ack: Ack = c2.post(LIVENESS_RESULTS, &res(&ia[2..])).await.unwrap();
    assert_eq!(ack.accepted, 4);
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM liveness_leases WHERE worker = 'w2'")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(held, 0, "answered leases are gone");
    // The server's own batch claims too: a worker gets nothing it is checking.
    let later = magnetar_core::now() + 10 * 86400;
    let rows = magnetar_server::liveness::claim(s.pool(), later, 100, "", 60)
        .await
        .unwrap();
    assert_eq!(rows.len(), 10);
    let none: Lease = c1.post(LEASE, &LeaseRequest { max: 100 }).await.unwrap();
    assert!(none.items.is_empty());
}

async fn triage_one(c: &Client, h: [u8; 20]) {
    let r: TriageResponse = c
        .post(
            TRIAGE,
            &TriageRequest {
                items: vec![TriageItem {
                    h: hex::encode(h),
                    via: "sample".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.fetch, vec![hex::encode(h)]);
}

fn meta(sighted: [u8; 20], raw: &[u8]) -> DhtResult {
    DhtResult {
        sighted: hex::encode(sighted),
        via: "sample".into(),
        peers_seen: 2,
        outcome: DhtOutcome::Metadata {
            raw: data_encoding::BASE64.encode(raw),
        },
    }
}

async fn upload(c: &Client, items: Vec<DhtResult>) -> DhtAck {
    c.post(DHT_RESULTS, &DhtResults { items }).await.unwrap()
}

async fn count(s: &PgStore, sql: &'static str, h: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(h)
        .fetch_one(s.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn uploads_are_verified_classified_and_screened_by_the_server() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _) = spawn(s, 900).await;
    let c1 = Client::new(&base, W1).unwrap();
    let c2 = Client::new(&base, W2).unwrap();

    // Dedup across workers: the second one is told to fetch nothing.
    let plain = info("Some.Film.2024.1080p", "0");
    let h = sha1(&plain);
    triage_one(&c1, h).await;
    let r: TriageResponse = c2
        .post(
            TRIAGE,
            &TriageRequest {
                items: vec![TriageItem {
                    h: hex::encode(h),
                    via: "announce".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(r.fetch.is_empty(), "already leased to w1");

    // A forged dict (another torrent's bytes under this hash) is refused, nothing stored.
    let other = info("Other.Film.2024.1080p", "0");
    let a = upload(&c1, vec![meta(h, &other)]).await;
    assert_eq!((a.rejected, a.stored), (1, 0));
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1",
            &hex::encode(h)
        )
        .await,
        0
    );
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1",
            &hex::encode(sha1(&other))
        )
        .await,
        0
    );
    let st: i16 = sqlx::query_scalar("SELECT status FROM dht_seen WHERE infohash = $1")
        .bind(h.as_slice())
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(st, 0, "still queued (leased)");
    // Bad base64 / bad hash: refused.
    let a = upload(
        &c1,
        vec![
            DhtResult {
                outcome: DhtOutcome::Metadata { raw: "@@@".into() },
                ..meta(h, &plain)
            },
            DhtResult {
                sighted: "xyz".into(),
                ..meta(h, &plain)
            },
        ],
    )
    .await;
    assert_eq!(a.rejected, 2);
    // The real dict is stored.
    let a = upload(&c1, vec![meta(h, &plain)]).await;
    assert_eq!((a.stored, a.rejected), (1, 0));

    // Adult classification is the server's: the wire has no category, an extra field claiming
    // one is ignored.
    let adult = info("Studio.XXX.Scene.2024.1080p", "0");
    let ha = sha1(&adult);
    triage_one(&c2, ha).await;
    let mut item = serde_json::to_value(meta(ha, &adult)).unwrap();
    item["category"] = json!("video");
    item["adult"] = json!(false);
    let (code, j) = post(&base, DHT_RESULTS, W2, Some("1"), json!({"items": [item]})).await;
    assert_eq!((code, j["stored"].as_u64()), (200, Some(1)));
    let (cat, ver): (String, i16) =
        sqlx::query_as("SELECT category, classifier_version FROM dht_torrents WHERE infohash = $1")
            .bind(hex::encode(ha))
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(
        (cat.as_str(), ver),
        ("porn", magnetar_core::classify::CLASSIFIER_VERSION)
    );

    // A worker that did not screen (old or broken): the server's blocklist stops it.
    let bad = info("Plain.Show.2024.720p", "zorblax");
    let hb = sha1(&bad);
    triage_one(&c1, hb).await;
    let a = upload(&c1, vec![meta(hb, &bad)]).await;
    assert_eq!((a.stored, a.blocked), (0, 1));
    let hb_hex = hex::encode(hb);
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1",
            &hb_hex
        )
        .await,
        0
    );
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM blocked_infohashes WHERE infohash = $1",
            &hb_hex
        )
        .await,
        1
    );

    // Hash-only block reports: applied for a hash the server handed out, ignored otherwise
    // (here a Nostr torrent's hash, never triaged): a worker cannot block the index.
    let hr = [0x42u8; 20];
    triage_one(&c2, hr).await;
    put(s, torrent(900, 1, NOW, 0x900, "a nostr torrent")).await;
    let blocked = |h: &str| DhtResult {
        sighted: h.into(),
        via: "sample".into(),
        peers_seen: 1,
        outcome: DhtOutcome::Blocked {
            v1: None,
            tier: "hard".into(),
            rule_version: 1,
        },
    };
    let a = upload(&c2, vec![blocked(&hex::encode(hr)), blocked(&ih(0x900))]).await;
    assert_eq!((a.blocked, a.ignored), (1, 1));
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM blocked_infohashes WHERE infohash = $1",
            &hex::encode(hr)
        )
        .await,
        1
    );
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM blocked_infohashes WHERE infohash = $1",
            &ih(0x900)
        )
        .await,
        0
    );
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_seen WHERE infohash = decode($1, 'hex')",
            &hex::encode(hr)
        )
        .await,
        0
    );

    // Failures back off like the server's own (attempt 1 → 1 h), only for queued hashes.
    let hf = [0x43u8; 20];
    triage_one(&c2, hf).await;
    let fail = DhtResult {
        sighted: hex::encode(hf),
        via: "sample".into(),
        peers_seen: 0,
        outcome: DhtOutcome::Failed {
            code: "no_peers".into(),
        },
    };
    let a = upload(
        &c2,
        vec![
            fail.clone(),
            DhtResult {
                outcome: DhtOutcome::Failed {
                    code: "bogus".into(),
                },
                sighted: hex::encode([0x44u8; 20]),
                ..fail.clone()
            },
        ],
    )
    .await;
    assert_eq!((a.applied, a.ignored), (1, 1));
    let (st, attempts, err): (i16, i16, Option<String>) =
        sqlx::query_as("SELECT status, attempts, last_error FROM dht_seen WHERE infohash = $1")
            .bind(hf.as_slice())
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!((st, attempts, err.as_deref()), (2, 1, Some("no_peers")));
    let a = upload(&c2, vec![fail]).await;
    assert_eq!(a.ignored, 1, "no longer queued");

    // Stats count what the server accepted.
    let st: J = http()
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w = &st["workers"];
    assert_eq!(w[0]["metadata_total"], 2, "{w}");
    assert_eq!(w[0]["rejected_total"], 3, "{w}");
    assert_eq!(w[1]["blocked_total"], 1, "{w}");
    assert_eq!(w[1]["stored_total"], 1, "{w}");
}

fn worker_dht(ports: Vec<u16>) -> DhtOptions {
    DhtOptions {
        mode: Mode::Aggressive,
        ports,
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
        screen: screen::global_screen(),
        upnp: None,
        service: None,
        geo: None,
        pacer: None,
        health: None,
    }
}

fn tweak(ip: Ipv4Addr, boot: SocketAddrV4) -> impl FnOnce(&mut EngineConfig) {
    move |c: &mut EngineConfig| {
        c.bind = ip;
        c.bootstrap = vec![boot];
        c.allow_loopback = true;
        c.query_timeout = Duration::from_millis(500);
        c.per_ip_qps = 100.0;
        c.response_per_ip = 1000.0;
        c.response_rate = 10_000.0;
        c.vote_responders = 3;
        c.vote_prefixes = 1;
    }
}

/// Two in-process workers share the seed checks and crawl the same fake DHT: every torrent is
/// fetched once, the blocklisted one reaches the server as a hash only, the adult one is
/// classified by the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_share_seed_checks_and_the_dht() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (tracker, scraped) = fake_tracker().await;
    put_torrents(s, 40, &tracker).await;
    let (base, bodies) = spawn(s, 900).await;

    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..6u8 {
        let ip = Ipv4Addr::new(127, 0, 39, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    let mut dicts = HashMap::new();
    let mut names = HashMap::new();
    for k in 0..10 {
        let raw = info(&format!("Fleet.Torrent.{k}.2024.720p"), "0");
        names.insert(sha1(&raw), "plain");
        dicts.insert(sha1(&raw), raw);
    }
    let blocked = info("Fleet.Show.Zorblax.2024.720p", "0");
    let hb = sha1(&blocked);
    names.insert(hb, "blocked");
    dicts.insert(hb, blocked);
    let adult = info("Studio.XXX.Fleet.Scene.2024.1080p", "0");
    let ha = sha1(&adult);
    names.insert(ha, "adult");
    dicts.insert(ha, adult);
    let hashes: Vec<[u8; 20]> = dicts.keys().copied().collect();
    let seeder_ip = Ipv4Addr::new(127, 0, 40, 1);
    let port = seeder(seeder_ip, dicts).await;
    let (client, _crx) = Dht::start(fake_cfg(seeder_ip, vec![net[0].1]))
        .await
        .unwrap();
    for (k, h) in hashes.iter().enumerate() {
        announce(&client, net[k % net.len()].1, *h, port).await;
    }

    let lcfg = LivenessConfig {
        use_dht: false,
        tracker_rps: 100.0,
        tracker_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let o1 = opts(&base, W1, Some(worker_dht(vec![0])), Some(lcfg.clone()));
    let o2 = opts(&base, W2, Some(worker_dht(vec![0])), Some(lcfg));
    let (d1, d2) = (o1.state_dir.clone(), o2.state_dir.clone());
    let budget = Some(Duration::from_secs(12));
    let boot0 = net[0].1;
    let r1 = tokio::spawn(worker::run(
        o1,
        stop.clone(),
        budget,
        tweak(Ipv4Addr::new(127, 0, 41, 1), boot0),
    ));
    let r2 = tokio::spawn(worker::run(
        o2,
        stop,
        budget,
        tweak(Ipv4Addr::new(127, 0, 42, 1), boot0),
    ));
    let (r1, r2) = (r1.await.unwrap().unwrap(), r2.await.unwrap().unwrap());
    client.shutdown().await;
    let rep = serde_json::to_string(&(&r1, &r2)).unwrap();
    eprintln!("{rep}");
    assert!(r1.error.is_none() && r2.error.is_none(), "{rep}");

    // Seed checks: all 40 done between the two, each written as the server's own.
    let l1 = r1.liveness.as_ref().unwrap();
    let l2 = r2.liveness.as_ref().unwrap();
    assert_eq!(l1.accepted + l2.accepted, 40, "{rep}");
    assert_eq!(l1.ignored + l2.ignored, 0);
    assert!(
        l1.accepted > 0 && l2.accepted > 0,
        "both took a share: {rep}"
    );
    let checked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM liveness l JOIN liveness_schedule s USING (infohash)
         WHERE l.seeders = 5 AND s.checked_at IS NOT NULL",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    assert_eq!(checked, 40);
    assert!(scraped.load(Ordering::SeqCst) >= 40);

    // DHT: every torrent fetched once across both workers.
    let mut uploads: HashMap<String, usize> = HashMap::new();
    for (path, body) in bodies.lock().unwrap().iter() {
        if path != DHT_RESULTS {
            continue;
        }
        let up: DhtResults = serde_json::from_slice(body).unwrap();
        for it in up.items {
            if let DhtOutcome::Metadata { raw } = &it.outcome {
                *uploads.entry(it.sighted.clone()).or_default() += 1;
                let raw = data_encoding::BASE64.decode(raw.as_bytes()).unwrap();
                let text = String::from_utf8_lossy(&raw).to_lowercase();
                assert!(
                    !text.contains("zorblax"),
                    "a blocklisted name left a worker"
                );
            }
            if it.sighted == hex::encode(hb) {
                assert!(
                    matches!(
                        it.outcome,
                        DhtOutcome::Blocked { .. } | DhtOutcome::Failed { .. }
                    ),
                    "the blocked torrent travels as a hash only"
                );
            }
        }
    }
    assert!(
        uploads.values().all(|n| *n == 1),
        "fetched twice: {uploads:?}"
    );
    let stored: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT infohash, category FROM dht_torrents")
            .fetch_all(s.pool())
            .await
            .unwrap()
            .into_iter()
            .collect();
    assert!(stored.len() >= 9, "{} stored: {rep}", stored.len());
    assert!(!stored.contains_key(&hex::encode(hb)));
    let both = r1.dht.as_ref().unwrap().blocked_here + r2.dht.as_ref().unwrap().blocked_here;
    if both > 0 {
        let n: i64 = count(
            s,
            "SELECT count(*) FROM blocked_infohashes WHERE infohash = $1",
            &hex::encode(hb),
        )
        .await;
        assert_eq!(n, 1, "the server recorded the hash");
    }
    if let Some(c) = stored.get(&hex::encode(ha)) {
        assert_eq!(c, "porn", "classified by the server");
    }
    assert_eq!(
        stored.len() as u64,
        r1.dht.as_ref().unwrap().server_stored + r2.dht.as_ref().unwrap().server_stored
    );
    // Both crawled (each got a share of the fetches) and saved their DHT state.
    assert!(d1.join("dht.json").exists() && d2.join("dht.json").exists());
    assert!(d1.join("heartbeat").exists());
    let st: J = http()
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w = st["workers"].as_array().unwrap();
    assert!(
        w.iter()
            .all(|w| w["connected"] == true && w["dht"]["mode"] == "aggressive"),
        "{w:?}"
    );
    let total: u64 = w
        .iter()
        .map(|w| w["liveness_results_total"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 40);
    let _ = names;
    for d in [d1, d2] {
        let _ = std::fs::remove_dir_all(d);
    }
}
