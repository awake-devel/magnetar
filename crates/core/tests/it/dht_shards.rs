//! The engine split per virtual node (D106, docs/DHT.md §20): the global brakes hold across
//! shards (total QPS cap, 1 q/s per remote IP), the pacer gives a cancelled query's turn to the
//! next one, and lookups still end and find peers with many virtual nodes.
#![cfg(feature = "dht-crawl")]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use magnetar_core::dht::id::distance;
use magnetar_core::dht::krpc::{self, Body, CompactNode, Message, Query, Response};
use magnetar_core::dht::{Dht, EngineConfig, Mode, QueryError, collect_peers};
use tokio::net::UdpSocket;
use tokio::time::Instant;

/// A minimal answering node: every query gets its 8 closest table entries; nodes within the
/// first id byte of the hash also give `peers`. Counts the queries it received.
async fn fake_node(
    me: CompactNode,
    table: Arc<Vec<CompactNode>>,
    peers: Vec<SocketAddrV4>,
    seen: Arc<AtomicU64>,
) {
    let sock = UdpSocket::bind(me.addr).await.expect("bind fake node");
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            let Ok((n, SocketAddr::V4(src))) = sock.recv_from(&mut buf).await else {
                continue;
            };
            seen.fetch_add(1, Ordering::Relaxed);
            let Ok(msg) = krpc::decode(&buf[..n]) else {
                continue;
            };
            let Body::Query { query, .. } = msg.body else {
                continue;
            };
            let target = match query {
                Query::GetPeers { info_hash } => info_hash,
                Query::FindNode { target } => target,
                _ => me.id,
            };
            let mut near: Vec<CompactNode> = table
                .iter()
                .copied()
                .filter(|c| c.addr != me.addr)
                .collect();
            near.sort_by_key(|c| distance(&c.id, &target));
            near.truncate(8);
            let values = if distance(&me.id, &target)[0] == 0 {
                peers.clone()
            } else {
                Vec::new()
            };
            let wire = krpc::encode(&Message {
                tid: msg.tid,
                body: Body::Response(Response {
                    id: me.id,
                    nodes: near,
                    values,
                    token: Some(vec![1]),
                    ..Default::default()
                }),
                ip: Some(src),
                version: None,
            });
            let _ = sock.send_to(&wire, SocketAddr::V4(src)).await;
        }
    });
}

/// `n` fake nodes on 127.0.<c>.x, ids random except that `close` of them sit next to `hash`.
async fn fake_net(
    c: u8,
    n: u8,
    hash: [u8; 20],
    close: u8,
    peers: Vec<SocketAddrV4>,
) -> (Vec<CompactNode>, Arc<AtomicU64>) {
    let table: Vec<CompactNode> = (1..=n)
        .map(|i| {
            let mut id: [u8; 20] = rand::random();
            if i <= close {
                id[..2].copy_from_slice(&hash[..2]);
            }
            CompactNode {
                id,
                addr: SocketAddrV4::new(Ipv4Addr::new(127, 0, c, i), 17_000),
            }
        })
        .collect();
    let table = Arc::new(table);
    let seen = Arc::new(AtomicU64::new(0));
    for me in table.iter().copied() {
        fake_node(me, table.clone(), peers.clone(), seen.clone()).await;
    }
    ((*table).clone(), seen)
}

fn crawler_cfg(ip: Ipv4Addr, nodes: usize, qps: u32) -> EngineConfig {
    let mut c = EngineConfig::new(Mode::Aggressive);
    c.bind = ip;
    c.ports = vec![0; nodes];
    c.qps = qps;
    c.pool_cap = 10_000;
    c.allow_loopback = true;
    c.sample = false;
    c.query_timeout = Duration::from_millis(500);
    c.lookup_timeout = Duration::from_millis(500);
    c
}

/// The total cap holds over 16 virtual nodes, and queries dropped while they wait for their
/// turn (a lookup that ended) do not waste it: the old pace lost those turns and sent only
/// 73–75 % of the cap at 4 096 lookups (docs/DHT.md §20).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rate_cap_holds_across_shards_and_cancelled_turns_are_reused() {
    let (table, _) = fake_net(40, 200, rand::random(), 0, Vec::new()).await;
    let mut cfg = crawler_cfg(Ipv4Addr::new(127, 0, 41, 1), 16, 400);
    cfg.per_ip_qps = 10_000.0; // 200 fake addresses stand for the DHT
    let (dht, _rx) = Dht::start(cfg).await.unwrap();
    assert_eq!(dht.virtual_nodes(), 16);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let s0 = dht.stats().queries_sent;
    let t0 = Instant::now();
    let secs = 3.0;
    let deadline = t0 + Duration::from_secs_f64(secs);
    let table = Arc::new(table);
    let mut set = tokio::task::JoinSet::new();
    // 300 senders over every virtual node; each gives up on a query after 0–40 ms, so most
    // queries are dropped while they still wait for a turn (demand ≈ 10 × the cap).
    for k in 0..300usize {
        let d = dht.clone();
        let table = table.clone();
        set.spawn(async move {
            let mut i = k;
            while Instant::now() < deadline {
                i = i.wrapping_mul(31).wrapping_add(7);
                let to = table[i % table.len()].addr;
                let wait = Duration::from_millis((i % 41) as u64);
                let _ = tokio::time::timeout(wait, d.query(i % 16, to, Query::Ping)).await;
            }
        });
    }
    while set.join_next().await.is_some() {}
    let el = t0.elapsed().as_secs_f64();
    let sent = (dht.stats().queries_sent - s0) as f64;
    let cap = 400.0 * el + 16.0; // the rate over the window plus the pacer's small burst
    assert!(sent <= cap, "sent {sent} over {el:.2} s, cap {cap}");
    assert!(
        sent >= 0.9 * 400.0 * secs,
        "sent {sent} over {el:.2} s: cancelled turns were lost; {:?}",
        dht.stats()
    );
    dht.shutdown().await;
}

/// 1 q/s per remote IP (burst 2) across every virtual node: queries from 16 sockets at the same
/// address at once get 2 through, the rest are refused before anything is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_per_ip_limit_holds_across_shards() {
    let (table, seen) = fake_net(42, 1, rand::random(), 0, Vec::new()).await;
    let to = table[0].addr;
    let (dht, _rx) = Dht::start(crawler_cfg(Ipv4Addr::new(127, 0, 43, 1), 16, 1000))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = seen.load(Ordering::Relaxed);
    let mut set = tokio::task::JoinSet::new();
    for node in 0..16 {
        let d = dht.clone();
        set.spawn(async move { d.query(node, to, Query::Ping).await });
    }
    let mut ok = 0;
    let mut limited = 0;
    while let Some(r) = set.join_next().await {
        match r.unwrap() {
            Ok(_) => ok += 1,
            Err(QueryError::RateLimited) => limited += 1,
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    assert_eq!((ok, limited), (2, 14));
    assert_eq!(seen.load(Ordering::Relaxed) - before, 2);
    // One second later one more token.
    tokio::time::sleep(Duration::from_millis(1050)).await;
    assert!(dht.query(5, to, Query::Ping).await.is_ok());
    assert_eq!(
        dht.query(9, to, Query::Ping).await.err(),
        Some(QueryError::RateLimited)
    );
    dht.shutdown().await;
}

/// With 16 virtual nodes, lookups started from every node end on their own (closest nodes
/// answered) well inside their budget and find the peers the nodes near the hash know.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lookups_end_and_find_peers_with_many_shards() {
    let hash: [u8; 20] = rand::random();
    let peers = vec![
        SocketAddrV4::new(Ipv4Addr::new(127, 0, 46, 1), 6881),
        SocketAddrV4::new(Ipv4Addr::new(127, 0, 46, 2), 6881),
    ];
    let (table, _) = fake_net(44, 120, hash, 6, peers.clone()).await;
    let mut cfg = crawler_cfg(Ipv4Addr::new(127, 0, 45, 1), 16, 2000);
    cfg.per_ip_qps = 1000.0;
    cfg.bootstrap = table.iter().skip(10).take(8).map(|c| c.addr).collect();
    let (dht, _rx) = Dht::start(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let d = dht.clone();
        set.spawn(async move {
            let t = Instant::now();
            let c = collect_peers(&d, hash, None, None, Duration::from_secs(5), 24).await;
            (c, t.elapsed())
        });
    }
    while let Some(r) = set.join_next().await {
        let (c, el) = r.unwrap();
        assert!(c.looked_up, "{c:?}");
        let mut got = c.peers.clone();
        got.sort();
        assert_eq!(got, peers, "{c:?}");
        assert!(el < Duration::from_secs(4), "lookup took {el:?}");
    }
    let st = dht.stats();
    assert_eq!(st.lookups, 32, "{st:?}");
    assert!(st.routing.iter().all(|&n| n > 0), "{st:?}");
    dht.shutdown().await;
}
