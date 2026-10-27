//! Iterative lookups against scripted UDP nodes on loopback (D76): silent nodes no longer hold
//! a walk for the hard query timeout (branch widening after the lookup timeout), late answers
//! still count, values that cannot be used (node farms, LAN addresses) do not end a walk early,
//! and a sampling node that answers after the lookup timeout still provides the peers.
#![cfg(feature = "dht-crawl")]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use magnetar_core::dht::krpc::{self, Body, CompactNode, Id, Message, Response};
use magnetar_core::dht::lookup::{Method, lookup};
use magnetar_core::dht::{Dht, EngineConfig, Mode, collect_peers};
use tokio::net::UdpSocket;

#[derive(Clone)]
struct Script {
    /// The id it answers with.
    id: Id,
    /// None: never answers.
    delay: Option<Duration>,
    values: Vec<SocketAddrV4>,
}

/// A node on its own loopback address that answers every query as scripted.
async fn node(ip: Ipv4Addr, s: Script) -> SocketAddrV4 {
    let sock = UdpSocket::bind((ip, 0)).await.unwrap();
    let addr = match sock.local_addr().unwrap() {
        SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let sock = std::sync::Arc::new(sock);
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            let Ok(m) = krpc::decode(&buf[..n]) else {
                continue;
            };
            let Some(delay) = s.delay else { continue };
            let wire = krpc::encode(&Message {
                tid: m.tid,
                body: Body::Response(Response {
                    id: s.id,
                    values: s.values.clone(),
                    token: Some(b"tok".to_vec()),
                    ..Default::default()
                }),
                ip: None,
                version: None,
            });
            let sock = sock.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = sock.send_to(&wire, from).await;
            });
        }
    });
    addr
}

async fn crawler(ip: Ipv4Addr, soft: Duration) -> Dht {
    let mut c = EngineConfig::new(Mode::Polite);
    c.bind = ip;
    c.ports = vec![0];
    c.qps = 1000;
    c.allow_loopback = true;
    c.sample = false;
    c.query_timeout = Duration::from_secs(2);
    c.lookup_timeout = soft;
    c.per_ip_qps = 100.0;
    let (d, _rx) = Dht::start(c).await.unwrap();
    d
}

/// An id sharing `bits` leading bits with `t` (then differing), last byte `tag`.
fn near(t: &Id, bits: usize, tag: u8) -> Id {
    let mut id = *t;
    id[bits / 8] ^= 0x80 >> (bits % 8);
    id[19] = tag;
    id
}

fn peer(s: &str) -> SocketAddrV4 {
    s.parse().unwrap()
}

/// 4 silent nodes closest to the target, 4 answering ones behind them. Before D76 the walk
/// waited the full query timeout for the silent ones; now they turn slow after the lookup
/// timeout and the walk ends on the answering ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_nodes_do_not_hold_the_walk() {
    let t: Id = rand::random();
    let mut seeds = Vec::new();
    for i in 0..4u8 {
        let id = near(&t, 63, i);
        let a = node(
            Ipv4Addr::new(127, 0, 30, 10 + i),
            Script {
                id,
                delay: None,
                values: vec![],
            },
        )
        .await;
        seeds.push(CompactNode { id, addr: a });
    }
    for i in 0..4u8 {
        let id = near(&t, 20, i);
        let a = node(
            Ipv4Addr::new(127, 0, 30, 20 + i),
            Script {
                id,
                delay: Some(Duration::ZERO),
                values: vec![],
            },
        )
        .await;
        seeds.push(CompactNode { id, addr: a });
    }
    let budget = Duration::from_secs(10);
    let d = crawler(Ipv4Addr::new(127, 0, 30, 1), Duration::from_millis(150)).await;
    let t0 = Instant::now();
    let r = lookup(&d, 0, t, Method::GetPeers, seeds.clone(), vec![], budget, 0).await;
    let fast = t0.elapsed();
    assert_eq!((r.queried, r.answered, r.slow, r.late), (8, 4, 4, 0));
    assert!(fast < Duration::from_millis(1000), "{fast:?}");
    let st = d.stats();
    assert_eq!((st.lookups, st.lookup_queries, st.lookup_slow), (1, 8, 4));
    d.shutdown().await;

    // Without widening (lookup timeout = query timeout) the same walk waits for the silent
    // nodes' hard timeout.
    let d = crawler(Ipv4Addr::new(127, 0, 30, 2), Duration::from_secs(2)).await;
    let t0 = Instant::now();
    let r = lookup(&d, 0, t, Method::GetPeers, seeds, vec![], budget, 0).await;
    assert!(t0.elapsed() >= Duration::from_secs(2), "{:?}", t0.elapsed());
    assert_eq!(r.answered, 4);
    d.shutdown().await;
}

/// A slow node's answer that comes before the hard timeout is still used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_answers_still_count() {
    let t: Id = rand::random();
    let id = near(&t, 30, 1);
    let a = node(
        Ipv4Addr::new(127, 0, 31, 10),
        Script {
            id,
            delay: Some(Duration::from_millis(400)),
            values: vec![peer("8.8.8.8:6881")],
        },
    )
    .await;
    let d = crawler(Ipv4Addr::new(127, 0, 31, 1), Duration::from_millis(100)).await;
    let seeds = vec![CompactNode { id, addr: a }];
    let r = lookup(
        &d,
        0,
        t,
        Method::GetPeers,
        seeds,
        vec![],
        Duration::from_secs(5),
        0,
    )
    .await;
    assert_eq!((r.slow, r.late, r.answered), (1, 1, 1));
    assert_eq!(r.peers, vec![peer("8.8.8.8:6881")]);
    assert_eq!(d.stats().lookup_late, 1);
    d.shutdown().await;
}

/// Bug fixed with D76: the early stop ("enough peers") counted every value, also those of a
/// node-farm answer (ignored in the result) and non-global ones (dropped before the fetcher),
/// so such answers could end a walk with no usable peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unusable_values_do_not_end_a_walk_early() {
    let t: Id = rand::random();
    let farm_seed = near(&t, 40, 1);
    let farm = node(
        Ipv4Addr::new(127, 0, 32, 10),
        Script {
            // Answers with an id made for the target (≥ 64 shared bits).
            id: near(&t, 100, 1),
            delay: Some(Duration::ZERO),
            values: (1..=6).map(|i| peer(&format!("8.8.8.{i}:1"))).collect(),
        },
    )
    .await;
    let lan_id = near(&t, 41, 2);
    let lan = node(
        Ipv4Addr::new(127, 0, 32, 11),
        Script {
            id: lan_id,
            delay: Some(Duration::ZERO),
            values: (1..=6).map(|i| peer(&format!("10.0.0.{i}:1"))).collect(),
        },
    )
    .await;
    let real_id = near(&t, 42, 3);
    let real = node(
        Ipv4Addr::new(127, 0, 32, 12),
        Script {
            id: real_id,
            delay: Some(Duration::from_millis(150)),
            values: vec![peer("9.9.9.9:1"), peer("9.9.9.10:1")],
        },
    )
    .await;
    let d = crawler(Ipv4Addr::new(127, 0, 32, 1), Duration::from_secs(1)).await;
    let seeds = vec![
        CompactNode {
            id: farm_seed,
            addr: farm,
        },
        CompactNode {
            id: lan_id,
            addr: lan,
        },
        CompactNode {
            id: real_id,
            addr: real,
        },
    ];
    let r = lookup(
        &d,
        0,
        t,
        Method::GetPeers,
        seeds,
        vec![],
        Duration::from_secs(5),
        2,
    )
    .await;
    assert_eq!(r.answered, 3, "waited for the real swarm");
    assert!(r.peers.contains(&peer("9.9.9.9:1")), "{:?}", r.peers);
    assert!(!r.peers.contains(&peer("8.8.8.1:1")), "farm values ignored");
    d.shutdown().await;
}

/// The sampling node is asked with the lookup timeout too: past it the lookup starts, and the
/// node's late answer still provides the peers (here the walk has nobody to ask).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_sampling_node_still_gives_its_peers() {
    let t: Id = rand::random();
    let src = node(
        Ipv4Addr::new(127, 0, 33, 10),
        Script {
            id: rand::random(),
            delay: Some(Duration::from_millis(300)),
            values: vec![peer("9.9.9.8:2")],
        },
    )
    .await;
    let d = crawler(Ipv4Addr::new(127, 0, 33, 1), Duration::from_millis(100)).await;
    let c = collect_peers(&d, t, None, Some(src), Duration::from_secs(5), 12).await;
    assert!(c.from_source && c.looked_up, "{c:?}");
    assert_eq!(c.peers, vec![peer("9.9.9.8:2")]);
    // Silent: nothing, bounded by the hard timeout.
    let silent = node(
        Ipv4Addr::new(127, 0, 33, 11),
        Script {
            id: rand::random(),
            delay: None,
            values: vec![],
        },
    )
    .await;
    // (A fresh crawler: this one now knows the first node, which would answer the walk.)
    d.shutdown().await;
    let d = crawler(Ipv4Addr::new(127, 0, 33, 2), Duration::from_millis(100)).await;
    let t0 = Instant::now();
    let c = collect_peers(&d, t, None, Some(silent), Duration::from_secs(5), 12).await;
    assert!(c.peers.is_empty() && !c.from_source);
    assert!(t0.elapsed() < Duration::from_secs(3));
    d.shutdown().await;
}

/// A query waiting for the global pace is not slow yet: the soft timeout runs from the send
/// (the first D76 build started it at the queueing, and with many lookups behind the pace
/// almost every query went slow before it left).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_soft_timeout_starts_at_the_send() {
    let t: Id = rand::random();
    let mut seeds = Vec::new();
    for i in 0..4u8 {
        let id = near(&t, 20 + i as usize, i);
        let a = node(
            Ipv4Addr::new(127, 0, 34, 10 + i),
            Script {
                id,
                delay: Some(Duration::from_millis(20)),
                values: vec![],
            },
        )
        .await;
        seeds.push(CompactNode { id, addr: a });
    }
    let mut c = EngineConfig::new(Mode::Polite);
    c.bind = Ipv4Addr::new(127, 0, 34, 1);
    c.ports = vec![0];
    c.qps = 5; // one send every 200 ms: the 4th query waits ~600 ms for its slot
    c.allow_loopback = true;
    c.sample = false;
    c.query_timeout = Duration::from_secs(2);
    c.lookup_timeout = Duration::from_millis(150);
    c.per_ip_qps = 100.0;
    let (d, _rx) = Dht::start(c).await.unwrap();
    let r = lookup(
        &d,
        0,
        t,
        Method::GetPeers,
        seeds,
        vec![],
        Duration::from_secs(5),
        0,
    )
    .await;
    assert_eq!((r.answered, r.slow), (4, 0), "queued queries went slow");
    d.shutdown().await;
}

/// Queries waiting for the pace do not hold transactions: many queued at a low rate never
/// fill the pending map (cap QPS × timeout × 2), so none is refused as busy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pace_queue_does_not_fill_the_transaction_map() {
    let mut targets = Vec::new();
    for i in 0..4u8 {
        targets.push(
            node(
                Ipv4Addr::new(127, 0, 35, 10 + i),
                Script {
                    id: rand::random(),
                    delay: Some(Duration::ZERO),
                    values: vec![],
                },
            )
            .await,
        );
    }
    let mut c = EngineConfig::new(Mode::Polite);
    c.bind = Ipv4Addr::new(127, 0, 35, 1);
    c.ports = vec![0];
    c.qps = 100; // pending cap = 100 × 1 s × 2 = 200
    c.allow_loopback = true;
    c.sample = false;
    c.query_timeout = Duration::from_secs(1);
    c.per_ip_qps = 1000.0;
    let (d, _rx) = Dht::start(c).await.unwrap();
    let qs = (0..300).map(|k| {
        let d = d.clone();
        let to = targets[k % 4];
        async move { d.query(0, to, magnetar_core::dht::krpc::Query::Ping).await }
    });
    let rs = futures::future::join_all(qs).await;
    assert_eq!(d.stats().busy, 0);
    assert!(
        rs.iter().all(|r| r.is_ok()),
        "{:?}",
        rs.iter().find(|r| r.is_err())
    );
    d.shutdown().await;
}
