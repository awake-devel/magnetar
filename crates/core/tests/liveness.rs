//! Liveness against in-process fakes: bencode, BEP 15 UDP tracker, BEP 48 HTTP tracker, BEP 33
//! Bloom vector and DHT walk, check_batch aggregation/TTL, run_due.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::liveness::dht::{self, Bloom256};
use magnetar_core::liveness::udp::{self, ScrapeStats};
use magnetar_core::liveness::{self, LivenessConfig, TTL_ALIVE, TTL_DEAD, TTL_ERROR, http};
use magnetar_core::model::LivenessSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const T: Duration = Duration::from_millis(400);

fn h(seed: u8) -> [u8; 20] {
    std::array::from_fn(|i| seed.wrapping_add(i as u8 * 7))
}

fn bytes(s: &[u8]) -> Value {
    Value::Bytes(s.to_vec())
}

// ---------------------------------------------------------------- bencode

#[test]
fn bencode_round_trip() {
    let v = Value::Dict(vec![
        (b"a".to_vec(), Value::Int(-42)),
        (
            b"b".to_vec(),
            Value::List(vec![bytes(b"spam"), Value::Int(0), Value::List(vec![])]),
        ),
        (
            b"c".to_vec(),
            Value::Dict(vec![(b"x".to_vec(), bytes(&[0, 255, 10]))]),
        ),
    ]);
    let enc = bencode::encode(&v);
    assert_eq!(
        enc,
        b"d1:ai-42e1:bl4:spami0elee1:cd1:x3:\x00\xff\x0aee".to_vec()
    );
    assert_eq!(bencode::decode(&enc).unwrap(), v);
}

#[test]
fn bencode_unsorted_dict_is_accepted_and_encoded_sorted() {
    // nyaa answers with `complete, incomplete, downloaded`.
    let raw = b"d8:completei5e10:incompletei2e10:downloadedi9ee\n";
    let v = bencode::decode(raw).unwrap();
    assert_eq!(v.get(b"complete").and_then(Value::as_int), Some(5));
    assert_eq!(v.get(b"incomplete").and_then(Value::as_int), Some(2));
    assert_eq!(v.get(b"downloaded").and_then(Value::as_int), Some(9));
    assert_eq!(
        bencode::encode(&v),
        b"d8:completei5e10:downloadedi9e10:incompletei2ee".to_vec()
    );
}

#[test]
fn bencode_rejects_malformed() {
    for bad in [
        &b""[..],
        b"i12",
        b"ie",
        b"i1x2e",
        b"5:abc",
        b"l",
        b"d3:keye",
        b"i1ei2e",
        b"x",
        b"-1:a",
        b"i99999999999999999999e",
    ] {
        assert!(
            bencode::decode(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    let deep = [vec![b'l'; 1000], vec![b'e'; 1000]].concat();
    assert!(bencode::decode(&deep).is_err());
}

// ---------------------------------------------------------------- fake UDP tracker

#[derive(Clone, Copy, Default)]
struct UdpBehavior {
    /// Max entries answered per scrape (real trackers: 75).
    cap: usize,
    /// Send a wrong-tid datagram and a runt before each real answer.
    noise: bool,
    /// Ignore the first connect request (exercises the retry).
    drop_first_connect: bool,
    /// Answer scrapes with action 3.
    error: bool,
    /// Added to every seeders count.
    offset: u32,
}

#[derive(Default)]
struct UdpLog {
    connects: AtomicUsize,
    scrapes: AtomicUsize,
    max_hashes: AtomicUsize,
}

fn fake_stats(hash: &[u8], offset: u32) -> ScrapeStats {
    ScrapeStats {
        seeders: hash[0] as u32 + offset,
        leechers: hash[1] as u32,
        completed: hash[2] as u32,
    }
}

async fn fake_udp_tracker(b: UdpBehavior) -> (String, Arc<UdpLog>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let url = format!("udp://{}/announce", sock.local_addr().unwrap());
    let log = Arc::new(UdpLog::default());
    let l = log.clone();
    tokio::spawn(async move {
        let conn_id: u64 = 0x1122_3344_5566_7788;
        let mut buf = [0u8; 4096];
        let mut dropped = false;
        loop {
            let (n, src) = sock.recv_from(&mut buf).await.unwrap();
            let req = &buf[..n];
            let action = u32::from_be_bytes(req[8..12].try_into().unwrap());
            let tid = &req[12..16];
            let mut resp = Vec::new();
            if action == 0 {
                assert_eq!(&req[..8], &0x41727101980u64.to_be_bytes());
                l.connects.fetch_add(1, Ordering::SeqCst);
                if b.drop_first_connect && !dropped {
                    dropped = true;
                    continue;
                }
                resp.extend_from_slice(&0u32.to_be_bytes());
                resp.extend_from_slice(tid);
                resp.extend_from_slice(&conn_id.to_be_bytes());
            } else if action == 2 {
                assert_eq!(&req[..8], &conn_id.to_be_bytes(), "connection id reused");
                l.scrapes.fetch_add(1, Ordering::SeqCst);
                let hashes: Vec<&[u8]> = req[16..].chunks(20).collect();
                l.max_hashes.fetch_max(hashes.len(), Ordering::SeqCst);
                if b.error {
                    resp.extend_from_slice(&3u32.to_be_bytes());
                    resp.extend_from_slice(tid);
                    resp.extend_from_slice(b"unregistered torrent");
                } else {
                    resp.extend_from_slice(&2u32.to_be_bytes());
                    resp.extend_from_slice(tid);
                    for hash in hashes.iter().take(b.cap) {
                        let s = fake_stats(hash, b.offset);
                        resp.extend_from_slice(&s.seeders.to_be_bytes());
                        resp.extend_from_slice(&s.completed.to_be_bytes());
                        resp.extend_from_slice(&s.leechers.to_be_bytes());
                    }
                }
            } else {
                continue;
            }
            if b.noise {
                let mut wrong = resp.clone();
                wrong[4] ^= 0xff;
                sock.send_to(&wrong, src).await.unwrap();
                sock.send_to(&[1, 2, 3], src).await.unwrap();
            }
            sock.send_to(&resp, src).await.unwrap();
        }
    });
    (url, log)
}

#[tokio::test]
async fn udp_scrape_batches_of_74_with_noise() {
    let (url, log) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        noise: true,
        ..Default::default()
    })
    .await;
    let hashes: Vec<[u8; 20]> = (0..150u8).map(h).collect();
    let res = udp::scrape(&url, &hashes, T).await.unwrap();
    assert_eq!(res.len(), 150);
    for (r, hash) in res.iter().zip(&hashes) {
        assert_eq!(*r, Some(fake_stats(hash, 0)));
    }
    assert_eq!(
        log.connects.load(Ordering::SeqCst),
        1,
        "connection id reused"
    );
    assert_eq!(log.scrapes.load(Ordering::SeqCst), 3);
    assert_eq!(log.max_hashes.load(Ordering::SeqCst), udp::UDP_SCRAPE_BATCH);
}

#[tokio::test]
async fn udp_scrape_truncation_is_unanswered() {
    let (url, _) = fake_udp_tracker(UdpBehavior {
        cap: 10,
        ..Default::default()
    })
    .await;
    let hashes: Vec<[u8; 20]> = (0..80u8).map(h).collect();
    let res = udp::scrape(&url, &hashes, T).await.unwrap();
    assert_eq!(res.len(), 80);
    // Two packets (74 + 6); the first is cut at 10, the second fits.
    for (i, r) in res.iter().enumerate() {
        assert_eq!(r.is_some(), !(10..74).contains(&i), "position {i}");
    }
    assert_eq!(res[0], Some(fake_stats(&hashes[0], 0)));
    assert_eq!(res[79], Some(fake_stats(&hashes[79], 0)));
}

#[tokio::test]
async fn udp_scrape_error_action() {
    let (url, _) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        error: true,
        ..Default::default()
    })
    .await;
    let err = udp::scrape(&url, &[h(1)], T).await.unwrap_err();
    assert!(err.to_string().contains("unregistered torrent"), "{err}");
}

#[tokio::test]
async fn udp_scrape_retries_once() {
    let (url, log) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        drop_first_connect: true,
        ..Default::default()
    })
    .await;
    let res = udp::scrape(&url, &[h(9)], T).await.unwrap();
    assert_eq!(res, vec![Some(fake_stats(&h(9), 0))]);
    assert_eq!(log.connects.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn udp_scrape_timeout() {
    // Bound but silent.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let url = format!("udp://{}", silent.local_addr().unwrap());
    let start = std::time::Instant::now();
    assert!(
        udp::scrape(&url, &[h(1)], Duration::from_millis(200))
            .await
            .is_err()
    );
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(400) && took < Duration::from_secs(2),
        "{took:?}"
    );
}

#[tokio::test]
async fn udp_scrape_bad_urls() {
    assert!(
        udp::scrape("udp://example.org/announce", &[h(1)], T)
            .await
            .is_err()
    );
    assert!(
        udp::scrape("http://example.org:80/announce", &[h(1)], T)
            .await
            .is_err()
    );
    assert_eq!(
        udp::scrape("udp://x.invalid:1", &[], T).await.unwrap(),
        vec![]
    );
}

// ---------------------------------------------------------------- fake HTTP tracker

#[derive(Default)]
struct HttpLog {
    requests: AtomicUsize,
    max_hashes: AtomicUsize,
    paths: Mutex<Vec<String>>,
}

/// Hashes starting with 0xEE are unknown to the tracker (omitted from `files`); a `passkey=bad`
/// query gets a `failure reason`.
async fn fake_http_tracker(offset: u32) -> (String, Arc<HttpLog>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(HttpLog::default());
    let l = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            let l = l.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = conn.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let line = String::from_utf8_lossy(&req)
                    .lines()
                    .next()
                    .unwrap()
                    .to_string();
                let target = line.split(' ').nth(1).unwrap().to_string();
                l.requests.fetch_add(1, Ordering::SeqCst);
                l.paths.lock().unwrap().push(target.clone());
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let hashes: Vec<Vec<u8>> = query
                    .split('&')
                    .filter_map(|kv| kv.strip_prefix("info_hash="))
                    .map(|v| percent_encoding::percent_decode_str(v).collect())
                    .collect();
                l.max_hashes.fetch_max(hashes.len(), Ordering::SeqCst);
                let (status, body) = if !path.ends_with("/scrape") && !path.ends_with("/scrape.php")
                {
                    ("404 Not Found", b"nope".to_vec())
                } else if query.contains("passkey=bad") {
                    ("200 OK", b"d14:failure reason11:bad passkeye".to_vec())
                } else {
                    // Hand-written so the keys stay unsorted on the wire.
                    let mut b = b"d5:filesd".to_vec();
                    for hash in hashes.iter().filter(|h| h[0] != 0xee) {
                        let s = fake_stats(hash, offset);
                        b.extend_from_slice(b"20:");
                        b.extend_from_slice(hash);
                        b.extend_from_slice(
                            format!(
                                "d8:completei{}e10:incompletei{}e10:downloadedi{}ee",
                                s.seeders, s.leechers, s.completed
                            )
                            .as_bytes(),
                        );
                    }
                    b.extend_from_slice(b"e5:flagsd20:min_request_intervali1800eee");
                    ("200 OK", b)
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                conn.write_all(head.as_bytes()).await.unwrap();
                conn.write_all(&body).await.unwrap();
                let _ = conn.shutdown().await;
            });
        }
    });
    (base, log)
}

#[test]
fn scrape_url_derivation() {
    let cases = [
        (
            "http://nyaa.tracker.wf:7777/announce",
            Some("http://nyaa.tracker.wf:7777/scrape"),
        ),
        (
            "https://t.example/a/announce.php?passkey=abc",
            Some("https://t.example/a/scrape.php?passkey=abc"),
        ),
        (
            "http://t.example/announce?k=v#frag",
            Some("http://t.example/scrape?k=v"),
        ),
        (
            "HTTP://T.Example:80/x/announce",
            Some("http://t.example/x/scrape"),
        ),
        ("http://t.example/a", None),
        ("http://t.example/announce/x", None),
        ("http://t.example/", None),
        ("udp://t.example:1337/announce", None),
        ("not a url", None),
    ];
    for (announce, want) in cases {
        assert_eq!(http::scrape_url(announce).as_deref(), want, "{announce}");
    }
}

#[tokio::test]
async fn http_scrape_batches_and_unsorted_keys() {
    let (base, log) = fake_http_tracker(0).await;
    let client = magnetar_core::http::client(Duration::from_secs(5)).unwrap();
    // Raw bytes that need escaping (space, %, &, non-ASCII) must survive the round trip.
    let mut hashes: Vec<[u8; 20]> = (0..120u8).map(h).collect();
    hashes[3] = *b" %&=+?#/\x00\xffabcdefghij";
    hashes[5][0] = 0xee;
    let res = http::scrape(&client, &format!("{base}/announce?passkey=ok"), &hashes)
        .await
        .unwrap();
    assert_eq!(res.len(), 120);
    for (i, (r, hash)) in res.iter().zip(&hashes).enumerate() {
        let want = if i == 5 {
            ScrapeStats {
                seeders: 0,
                leechers: 0,
                completed: 0,
            }
        } else {
            fake_stats(hash, 0)
        };
        assert_eq!(*r, Some(want), "position {i}");
    }
    assert_eq!(log.requests.load(Ordering::SeqCst), 3);
    assert_eq!(
        log.max_hashes.load(Ordering::SeqCst),
        http::HTTP_SCRAPE_BATCH
    );
    let paths = log.paths.lock().unwrap();
    assert!(
        paths
            .iter()
            .all(|p| p.starts_with("/scrape?passkey=ok&info_hash=")),
        "{paths:?}"
    );
}

#[tokio::test]
async fn http_scrape_failure_reason_and_status() {
    let (base, _) = fake_http_tracker(0).await;
    let client = magnetar_core::http::client(Duration::from_secs(5)).unwrap();
    let err = http::scrape(&client, &format!("{base}/announce?passkey=bad"), &[h(1)])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("bad passkey"), "{err}");
    let err = http::scrape(&client, &format!("{base}/x/announce.php"), &[h(1)])
        .await
        .unwrap();
    assert_eq!(err, vec![Some(fake_stats(&h(1), 0))]);
    let err = http::scrape(&client, &format!("{base}/nothing"), &[h(1)]).await;
    assert!(err.is_err());
}

// ---------------------------------------------------------------- BEP 33

#[test]
fn bloom_bep33_test_vector() {
    let mut bf = Bloom256::new();
    for i in 0..=255u8 {
        bf.insert_ip(&IpAddr::V4(Ipv4Addr::new(192, 0, 2, i)));
    }
    for i in 0..=0x3e7u16 {
        bf.insert_ip(&IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, i)));
    }
    assert_eq!(
        &bf.0[..8],
        &[0xF6, 0xC3, 0xF5, 0xEA, 0xA0, 0x7F, 0xFD, 0x91]
    );
    let est = bf.estimate();
    assert!((est - 1224.93).abs() < 0.01, "{est}");
    assert_eq!(bf.count(), 1225);
}

#[test]
fn bloom_edges() {
    assert_eq!(Bloom256::new().count(), 0);
    assert_eq!(Bloom256([0xff; 256]).count(), dht::ESTIMATE_CAP);
    let mut one = Bloom256::new();
    one.insert_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    assert_eq!(one.count(), 1);
    let mut u = Bloom256::new();
    u.union(&one.0);
    assert_eq!(u, one);
}

/// A two-node fake DHT: the bootstrap node only knows a node closer to the target, which stores
/// the swarm (BF filters + values).
#[tokio::test]
async fn dht_walk_against_fake_nodes() {
    let target = h(42);
    let far = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let near = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let near_addr = near.local_addr().unwrap();
    let mut near_id = target;
    near_id[19] ^= 1;

    let mut seeds = Bloom256::new();
    let mut peers = Bloom256::new();
    for i in 0..50u8 {
        seeds.insert_ip(&IpAddr::V4(Ipv4Addr::new(10, 1, 0, i)));
    }
    for i in 0..200u8 {
        peers.insert_ip(&IpAddr::V4(Ipv4Addr::new(10, 2, 0, i)));
    }
    let seen_query = Arc::new(Mutex::new(None));

    let mut compact = near_id.to_vec();
    compact.extend_from_slice(&[127, 0, 0, 1]);
    compact.extend_from_slice(&near_addr.port().to_be_bytes());
    // A port-0 entry must be skipped.
    compact.extend_from_slice(&[7u8; 20]);
    compact.extend_from_slice(&[127, 0, 0, 1, 0, 0]);
    let far_reply = move |t: Value| {
        Value::Dict(vec![
            (b"t".to_vec(), t),
            (b"y".to_vec(), bytes(b"r")),
            (
                b"r".to_vec(),
                Value::Dict(vec![
                    (b"id".to_vec(), bytes(&[0xaa; 20])),
                    (b"token".to_vec(), bytes(b"tok")),
                    (b"nodes".to_vec(), bytes(&compact)),
                ]),
            ),
        ])
    };
    let values = vec![
        bytes(&[10, 2, 0, 1, 0x1a, 0xe1]),
        bytes(&[10, 3, 0, 1, 0x1a, 0xe1]),
    ];
    let (s, p) = (seeds.0.to_vec(), peers.0.to_vec());
    let near_reply = move |t: Value| {
        Value::Dict(vec![
            (b"y".to_vec(), bytes(b"r")),
            (b"t".to_vec(), t),
            (
                b"r".to_vec(),
                Value::Dict(vec![
                    (b"id".to_vec(), bytes(&near_id)),
                    (b"values".to_vec(), Value::List(values.clone())),
                    (b"BFsd".to_vec(), bytes(&s)),
                    (b"BFpe".to_vec(), bytes(&p)),
                ]),
            ),
        ])
    };
    fn serve(
        sock: UdpSocket,
        reply: impl Fn(Value) -> Value + Send + 'static,
        seen: Option<Arc<Mutex<Option<Value>>>>,
    ) {
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let (n, src) = sock.recv_from(&mut buf).await.unwrap();
                let q = bencode::decode(&buf[..n]).unwrap();
                let t = q.get(b"t").unwrap().clone();
                if let Some(seen) = &seen {
                    *seen.lock().unwrap() = Some(q);
                }
                sock.send_to(&bencode::encode(&reply(t)), src)
                    .await
                    .unwrap();
            }
        });
    }
    let far_addr = far.local_addr().unwrap();
    serve(far, far_reply, Some(seen_query.clone()));
    serve(near, near_reply, None);

    let start = std::time::Instant::now();
    let r = dht::scrape_from(&[far_addr], &target, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "walk converges early"
    );
    assert_eq!(r.nodes_queried, 2);
    assert_eq!(r.bf_responses, 1);
    assert_eq!(r.values_ips, 2);
    assert!((48..=52).contains(&r.seeds_est), "{r:?}");
    // 200 in BFpe plus 10.3.0.1 from values (10.2.0.1 is already in); Bloom collisions add noise.
    assert!((195..=215).contains(&r.peers_est), "{r:?}");
    assert!((240..=260).contains(&r.total_est), "{r:?}");

    let q = seen_query.lock().unwrap().clone().unwrap();
    assert_eq!(
        q.get(b"q").and_then(Value::as_bytes),
        Some(&b"get_peers"[..])
    );
    assert_eq!(q.get(b"y").and_then(Value::as_bytes), Some(&b"q"[..]));
    assert_eq!(q.get(b"ro").and_then(Value::as_int), Some(1));
    let a = q.get(b"a").unwrap();
    assert_eq!(a.get(b"scrape").and_then(Value::as_int), Some(1));
    assert_eq!(
        a.get(b"info_hash").and_then(Value::as_bytes),
        Some(&target[..])
    );
    assert_eq!(
        a.get(b"id").and_then(Value::as_bytes).map(<[u8]>::len),
        Some(20)
    );
}

#[tokio::test]
async fn dht_no_answer_is_an_error() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = silent.local_addr().unwrap();
    let r = dht::scrape_from(&[addr], &h(1), Duration::from_millis(500)).await;
    assert!(r.is_err());
}

// ---------------------------------------------------------------- check_batch / run_due

fn cfg() -> LivenessConfig {
    LivenessConfig {
        tracker_timeout: T,
        use_dht: false,
        ..Default::default()
    }
}

#[tokio::test]
async fn check_batch_aggregates_max_over_trackers() {
    let (udp_a, log_a) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        ..Default::default()
    })
    .await;
    let (udp_b, _) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        offset: 3,
        ..Default::default()
    })
    .await;
    let (http_base, http_log) = fake_http_tracker(7).await;
    let http_t = format!("{http_base}/announce");
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let dead_t = format!("udp://{}", silent.local_addr().unwrap());
    let udp_a_nopath = udp_a.trim_end_matches("/announce").to_string();

    let alive = h(10); // seeders 10 / 13 / 17
    let mut dead = h(0); // seeders 0 on A, but B (offset 3) and HTTP (offset 7) are not asked
    dead[0] = 0;
    let lonely = h(200);
    let items = vec![
        (
            hex::encode(alive),
            vec![
                udp_a.clone(),
                udp_a_nopath.clone(), // same tracker, grouped once
                udp_b.clone(),
                http_t.clone(),
                "wss://tracker.webtorrent.dev".into(),
                "http://t.example/no-scrape".into(),
                dead_t.clone(),
            ],
        ),
        (hex::encode(dead), vec![udp_a.clone()]),
        (hex::encode(lonely), vec![dead_t.clone()]),
        (hex::encode(h(77)), vec![]),
        ("not-a-hash".into(), vec![udp_a.clone()]),
        (hex::encode(alive), vec![]), // duplicate infohash: shares the first item's trackers
    ];
    let now = 1_700_000_000;
    let out = liveness::check_batch(&items, &cfg(), now).await;
    assert_eq!(out.len(), items.len());
    for (l, (ih, _)) in out.iter().zip(&items) {
        assert_eq!(&l.infohash, ih);
        assert_eq!(l.checked_at, now);
    }

    let a = &out[0];
    let want = fake_stats(&alive, 7);
    assert_eq!(a.seeders, Some(want.seeders));
    assert_eq!(a.leechers, Some(want.leechers));
    assert_eq!(a.completed, Some(want.completed));
    assert_eq!(a.source, LivenessSource::Http);
    assert_eq!((a.trackers_ok, a.trackers_failed), (3, 1));
    assert_eq!(a.expires_at, now + TTL_ALIVE);
    assert_eq!(a.dht_peers_est, None);

    let d = &out[1];
    assert_eq!(d.seeders, Some(0));
    assert_eq!(d.source, LivenessSource::Udp);
    assert_eq!(d.expires_at, now + TTL_DEAD);
    assert!(d.is_dead(now));

    for unknown in [&out[2], &out[3], &out[4]] {
        assert_eq!(unknown.source, LivenessSource::None);
        assert_eq!(unknown.seeders, None);
        assert_eq!(unknown.expires_at, now + TTL_ERROR);
        assert!(!unknown.is_dead(now), "unknown is never dead");
    }
    assert_eq!((out[2].trackers_ok, out[2].trackers_failed), (0, 1));
    assert_eq!((out[3].trackers_ok, out[3].trackers_failed), (0, 0));

    assert_eq!(out[5].seeders, a.seeders);
    // One connect + one scrape on A for both hashes it serves.
    assert_eq!(log_a.connects.load(Ordering::SeqCst), 1);
    assert_eq!(log_a.scrapes.load(Ordering::SeqCst), 1);
    assert_eq!(log_a.max_hashes.load(Ordering::SeqCst), 2);
    assert_eq!(http_log.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn check_batch_extra_trackers_apply_to_every_item() {
    let (udp_a, log) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        ..Default::default()
    })
    .await;
    let cfg = LivenessConfig {
        extra_trackers: vec![udp_a],
        ..cfg()
    };
    let items: Vec<(String, Vec<String>)> =
        (1..=3u8).map(|i| (hex::encode(h(i)), vec![])).collect();
    let out = liveness::check_batch(&items, &cfg, 1).await;
    for (l, i) in out.iter().zip(1..=3u8) {
        assert_eq!(l.seeders, Some(fake_stats(&h(i), 0).seeders));
        assert_eq!(l.trackers_ok, 1);
    }
    assert_eq!(log.scrapes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn check_batch_empty() {
    assert!(liveness::check_batch(&[], &cfg(), 1).await.is_empty());
}

mod fake_store {
    use magnetar_core::Result;
    use magnetar_core::model::*;
    use magnetar_core::query::{SearchPage, SearchQuery};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeStore {
        pub due: Vec<(String, Vec<String>)>,
        pub put: Mutex<Vec<Liveness>>,
    }

    #[allow(unused_variables)]
    #[async_trait::async_trait]
    impl magnetar_core::store::Store for FakeStore {
        async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool> {
            unimplemented!("put_torrent")
        }

        async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool> {
            unimplemented!("put_comment")
        }

        async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool> {
            unimplemented!("put_replaceable")
        }

        async fn put_report(&self, raw: &RawEvent) -> Result<bool> {
            unimplemented!("put_report")
        }

        async fn has_event(&self, id: &str) -> Result<bool> {
            unimplemented!("has_event")
        }

        async fn known_torrent_event(&self, id: &str) -> Result<bool> {
            unimplemented!("known_torrent_event")
        }

        async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
            unimplemented!("get_raw_events")
        }

        async fn upsert_relay(&self, url: &str, source: &str) -> Result<()> {
            unimplemented!("upsert_relay")
        }

        async fn list_relays(&self) -> Result<Vec<RelayInfo>> {
            unimplemented!("list_relays")
        }

        async fn record_relay_result(
            &self,
            url: &str,
            ok: bool,
            err: Option<&str>,
            found: u64,
            now: i64,
        ) -> Result<()> {
            unimplemented!("record_relay_result")
        }

        async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()> {
            unimplemented!("set_relay_enabled")
        }

        async fn set_relay_supports_neg(&self, url: &str, supports: bool) -> Result<()> {
            unimplemented!("set_relay_supports_neg")
        }

        async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>> {
            unimplemented!("get_cursor")
        }

        async fn put_cursor(&self, c: &Cursor) -> Result<()> {
            unimplemented!("put_cursor")
        }

        async fn publishers(&self, limit: usize) -> Result<Vec<String>> {
            unimplemented!("publishers")
        }

        async fn relay_lists_for(
            &self,
            pubkeys: &[String],
        ) -> Result<Vec<(String, String, bool, bool)>> {
            unimplemented!("relay_lists_for")
        }

        async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool> {
            unimplemented!("relay_list_oversized")
        }

        async fn search(&self, q: &SearchQuery, now: i64) -> Result<SearchPage> {
            unimplemented!("search")
        }

        async fn torrent_detail(&self, infohash: &str, now: i64) -> Result<Option<TorrentDetail>> {
            unimplemented!("torrent_detail")
        }

        async fn liveness_due(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>> {
            Ok(self.due.clone())
        }

        async fn put_liveness(&self, l: &Liveness) -> Result<()> {
            self.put.lock().unwrap().push(l.clone());
            Ok(())
        }

        async fn get_liveness(&self, infohash: &str) -> Result<Option<Liveness>> {
            unimplemented!("get_liveness")
        }

        async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>> {
            unimplemented!("follows_of")
        }

        async fn followers_among(
            &self,
            targets: &[String],
            among: &[String],
        ) -> Result<Vec<(String, Vec<String>)>> {
            unimplemented!("followers_among")
        }

        async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>> {
            unimplemented!("mutes_by")
        }

        async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>> {
            unimplemented!("reports_by")
        }

        async fn comment_authors_on(
            &self,
            publisher: &str,
            authors: &[String],
        ) -> Result<Vec<String>> {
            unimplemented!("comment_authors_on")
        }

        async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats> {
            unimplemented!("publisher_stats")
        }

        async fn put_trust(&self, s: &TrustScore) -> Result<()> {
            unimplemented!("put_trust")
        }

        async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>> {
            unimplemented!("get_trust")
        }

        async fn graph_version(&self) -> Result<i64> {
            unimplemented!("graph_version")
        }

        async fn bump_graph_version(&self) -> Result<i64> {
            unimplemented!("bump_graph_version")
        }

        async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()> {
            unimplemented!("set_override")
        }

        async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>> {
            unimplemented!("overrides")
        }

        async fn get_setting(&self, key: &str) -> Result<Option<String>> {
            unimplemented!("get_setting")
        }

        async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
            unimplemented!("set_setting")
        }

        async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
            unimplemented!("list_saved_searches")
        }

        async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64> {
            unimplemented!("put_saved_search")
        }

        async fn delete_saved_search(&self, id: i64) -> Result<()> {
            unimplemented!("delete_saved_search")
        }

        async fn stats(&self) -> Result<StoreStats> {
            unimplemented!("stats")
        }
    }
}

#[tokio::test]
async fn run_due_checks_and_stores() {
    let (udp_a, _) = fake_udp_tracker(UdpBehavior {
        cap: 75,
        ..Default::default()
    })
    .await;
    let store = fake_store::FakeStore {
        due: vec![
            (hex::encode(h(5)), vec![udp_a]),
            (hex::encode(h(6)), vec![]),
        ],
        ..Default::default()
    };
    let n = liveness::run_due(&store, &cfg(), 1000, 10).await.unwrap();
    assert_eq!(n, 2);
    let put = store.put.lock().unwrap().clone();
    assert_eq!(put.len(), 2);
    assert_eq!(put[0].seeders, Some(5));
    assert_eq!(put[0].expires_at, 1000 + TTL_ALIVE);
    assert_eq!(put[1].source, LivenessSource::None);

    let empty = fake_store::FakeStore::default();
    assert_eq!(
        liveness::run_due(&empty, &cfg(), 1000, 10).await.unwrap(),
        0
    );
}
