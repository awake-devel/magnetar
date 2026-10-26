//! The crawler against a local fake DHT: several in-process nodes, each on its own loopback
//! address (127.0.0.0/8 is routed to `lo` on Linux, so the per-IP rules apply as on the real
//! network). Only the `allow_loopback` test seam differs from production.
#![cfg(feature = "dht-crawl")]

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use magnetar_core::dht::krpc::Query;
use magnetar_core::dht::{Dht, EngineConfig, Mode, Sighting, Via, collect_peers};
use tokio::sync::mpsc;

fn cfg(mode: Mode, ip: Ipv4Addr, ports: usize, bootstrap: Vec<SocketAddrV4>) -> EngineConfig {
    let mut c = EngineConfig::new(mode);
    c.bind = ip;
    c.ports = vec![0; ports];
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
    c
}

async fn fake_network(n: u8) -> Vec<(Dht, SocketAddrV4)> {
    let mut nodes = Vec::new();
    let mut boot = Vec::new();
    for i in 0..n {
        let ip = Ipv4Addr::new(127, 0, 1, 10 + i);
        let mut c = cfg(Mode::Aggressive, ip, 1, boot.clone());
        c.sample = false;
        let (d, _rx) = Dht::start(c).await.expect("bind fake node");
        let addr = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(addr);
        }
        nodes.push((d, addr));
    }
    nodes
}

/// Announce `peer_port` for `hash` to `to` the way a BitTorrent client does (get_peers for a
/// token, then announce_peer). Test-only: the crawler itself never announces.
async fn announce(from: &Dht, to: SocketAddrV4, hash: [u8; 20], implied: bool) {
    let r = from
        .query(0, to, Query::GetPeers { info_hash: hash })
        .await
        .expect("get_peers");
    from.query(
        0,
        to,
        Query::AnnouncePeer {
            info_hash: hash,
            port: 7777,
            implied_port: implied,
            token: r.token.expect("token"),
        },
    )
    .await
    .expect("announce");
}

async fn drain_until(
    rx: &mut mpsc::Receiver<Sighting>,
    got: &mut Vec<Sighting>,
    done: impl Fn(&[Sighting]) -> bool,
    budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + budget;
    while !done(got) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(s)) => got.push(s),
            _ => break,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crawler_samples_captures_and_collects_peers() {
    let net = fake_network(12).await;
    // A client that has 30 torrents announces each to two network nodes.
    let client_ip = Ipv4Addr::new(127, 0, 2, 1);
    let mut cc = cfg(Mode::Aggressive, client_ip, 1, vec![net[0].1]);
    cc.sample = false;
    let (client, _crx) = Dht::start(cc).await.unwrap();
    let hashes: Vec<[u8; 20]> = (0..30).map(|_| rand::random()).collect();
    for (k, h) in hashes.iter().enumerate() {
        for j in [k % net.len(), (k * 7 + 3) % net.len()] {
            announce(&client, net[j].1, *h, k % 2 == 0).await;
        }
    }

    // The crawler: aggressive, two virtual nodes on one address, bootstrapped from node 0.
    let crawler_ip = Ipv4Addr::new(127, 0, 3, 1);
    let (crawler, mut rx) = Dht::start(cfg(Mode::Aggressive, crawler_ip, 2, vec![net[0].1]))
        .await
        .unwrap();
    let mut got = Vec::new();
    let want: HashSet<[u8; 20]> = hashes.iter().copied().collect();
    let all_sampled = |g: &[Sighting]| {
        let s: HashSet<_> = g
            .iter()
            .filter(|s| s.via == Via::Sample)
            .map(|s| s.info_hash)
            .collect();
        want.is_subset(&s)
    };
    drain_until(&mut rx, &mut got, all_sampled, Duration::from_secs(20)).await;
    let sampled: HashSet<_> = got
        .iter()
        .filter(|s| s.via == Via::Sample)
        .map(|s| s.info_hash)
        .collect();
    assert!(
        want.is_subset(&sampled),
        "sampled {} of {} hashes; stats {:?}",
        want.intersection(&sampled).count(),
        want.len(),
        crawler.stats()
    );
    // Every sample names the node that held it.
    assert!(
        got.iter()
            .filter(|s| s.via == Via::Sample)
            .all(|s| s.source.is_some())
    );

    let st = crawler.stats();
    // Every hash was already sampled; at least a few nodes must be known supporters by now.
    assert!(st.pool.bep51_yes >= 3, "{st:?}");
    assert!(st.routing.iter().all(|&n| n > 0), "{st:?}");
    // BEP 42 vote over the `ip` field of the answers.
    assert!(st.external_ip_known, "{st:?}");
    let id = crawler.identity().unwrap();
    assert_eq!(id.external_ip, crawler_ip);
    for nid in &id.ids {
        assert!(magnetar_core::dht::id::bep42_valid(crawler_ip, nid));
    }

    // Peer collection: from the sampling node directly.
    let s = got.iter().find(|s| s.via == Via::Sample).copied().unwrap();
    let c = collect_peers(
        &crawler,
        s.info_hash,
        None,
        s.source,
        Duration::from_secs(5),
        8,
    )
    .await;
    assert!(c.from_source && !c.peers.is_empty(), "{c:?}");
    assert!(c.peers.iter().all(|p| *p.ip() == client_ip));
    // And through an iterative lookup when no source is known.
    let c = collect_peers(&crawler, hashes[5], None, None, Duration::from_secs(5), 8).await;
    assert!(c.looked_up && !c.peers.is_empty(), "{c:?}");
    // An unknown hash: no peers, nothing invented.
    let c = collect_peers(
        &crawler,
        rand::random(),
        None,
        None,
        Duration::from_secs(3),
        8,
    )
    .await;
    assert!(c.peers.is_empty(), "{c:?}");

    // Passive capture: incoming get_peers and announce_peer to the crawler's own ports.
    let target = SocketAddrV4::new(crawler_ip, crawler.local_ports()[1]);
    let fresh: [u8; 20] = rand::random();
    announce(&client, target, fresh, false).await;
    let mut seen = Vec::new();
    drain_until(
        &mut rx,
        &mut seen,
        |g| {
            g.iter()
                .any(|s| s.info_hash == fresh && s.via == Via::Announce)
        },
        Duration::from_secs(5),
    )
    .await;
    let gp = seen
        .iter()
        .find(|s| s.info_hash == fresh && s.via == Via::GetPeers);
    let an = seen
        .iter()
        .find(|s| s.info_hash == fresh && s.via == Via::Announce)
        .expect("announce captured");
    assert!(gp.is_some());
    assert_eq!(an.peer, Some(SocketAddrV4::new(client_ip, 7777)));
    // The announced peer is served back honestly.
    let r = client
        .query(0, target, Query::GetPeers { info_hash: fresh })
        .await
        .unwrap();
    assert_eq!(r.values, vec![SocketAddrV4::new(client_ip, 7777)]);
    // A forged token is refused (error 203) and stores nothing.
    let bad = client
        .query(
            0,
            target,
            Query::AnnouncePeer {
                info_hash: rand::random(),
                port: 1,
                implied_port: false,
                token: b"forged!!".to_vec(),
            },
        )
        .await;
    assert_eq!(
        bad.unwrap_err(),
        magnetar_core::dht::QueryError::Remote(203)
    );
    assert!(crawler.stats().bad_tokens >= 1);

    crawler.shutdown().await;
    assert!(crawler.is_stopped());
    client.shutdown().await;
    for (d, _) in net {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn polite_node_is_read_only() {
    let net = fake_network(4).await;
    let ip = Ipv4Addr::new(127, 0, 4, 1);
    let mut c = cfg(Mode::Polite, ip, 1, vec![net[0].1]);
    c.sample = false;
    let (polite, _rx) = Dht::start(c).await.unwrap();
    // It can query...
    let r = polite
        .query(0, net[1].1, Query::Ping)
        .await
        .expect("ping answered");
    assert_ne!(r.id, [0; 20]);
    // ...but never answers, and the network does not route it (BEP 43 `ro=1`).
    let me = SocketAddrV4::new(ip, polite.local_ports()[0]);
    let e = net[2].0.query(0, me, Query::Ping).await;
    assert_eq!(e.unwrap_err(), magnetar_core::dht::QueryError::Timeout);
    assert!(polite.stats().incoming_ignored >= 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let routed: usize = net
        .iter()
        .map(|(d, _)| {
            d.closest(&polite.ids()[0], 64)
                .iter()
                .filter(|n| n.addr == me)
                .count()
        })
        .sum();
    assert_eq!(routed, 0);
    polite.shutdown().await;
    for (d, _) in net {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_datagrams_do_not_stop_the_node() {
    let net = fake_network(2).await;
    let (d, addr) = (&net[1].0, net[1].1);
    let sock = tokio::net::UdpSocket::bind("127.0.0.9:0").await.unwrap();
    let mut junk: Vec<Vec<u8>> = vec![
        vec![],
        b"d".to_vec(),
        b"d1:t2:aa1:y1:qe".to_vec(),
        vec![b'l'; 1500],
        vec![0xff; 4000], // longer than the 2 KiB buffer
        b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t9:123456789e".to_vec(),
        // A response nobody asked for.
        b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:zz1:y1:re".to_vec(),
    ];
    for _ in 0..200 {
        let n = rand::random_range(0..1800);
        junk.push((0..n).map(|_| rand::random()).collect());
    }
    for j in &junk {
        sock.send_to(j, addr).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Still answering.
    let r = net[0].0.query(0, addr, Query::Ping).await;
    assert!(r.is_ok(), "{r:?}");
    let st = d.stats();
    assert!(
        st.decode_errors > 0 && st.too_long >= 1 && st.unmatched >= 1,
        "{st:?}"
    );
    let mut by_kind: HashMap<&str, u64> = HashMap::new();
    by_kind.insert("decode", st.decode_errors);
    assert!(by_kind["decode"] >= 5);
    for (d, _) in net {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_queries_release_their_transactions() {
    let net = fake_network(1).await;
    let d = &net[0].0;
    // A socket that never answers.
    let silent = tokio::net::UdpSocket::bind("127.0.0.77:0").await.unwrap();
    let to = match silent.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    for _ in 0..20 {
        // Dropped mid-flight, the way an early-stopping lookup drops its queries.
        let _ = tokio::time::timeout(Duration::from_millis(20), d.query(0, to, Query::Ping)).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(d.stats().pending, 0, "{:?}", d.stats());
    // And a completed query leaves nothing either.
    let r = d.query(0, net[0].1, Query::Ping).await;
    assert_eq!(
        r.unwrap_err(),
        magnetar_core::dht::QueryError::Filtered,
        "own port"
    );
    assert_eq!(d.stats().pending, 0);
    for (d, _) in net {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sampling_can_be_paused_and_throttled() {
    let net = fake_network(4).await;
    let ip = Ipv4Addr::new(127, 0, 9, 1);
    let (d, _rx) = Dht::start(cfg(Mode::Aggressive, ip, 1, vec![net[0].1]))
        .await
        .unwrap();
    d.set_sample_share(0.0);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let st = d.stats();
    assert_eq!(st.sent_sample_infohashes, 0, "{st:?}");
    assert_eq!(st.sample_share, 0.0);
    // Lookups still work while sampling is paused.
    assert!(d.query(0, net[1].1, Query::Ping).await.is_ok());
    // A 1 % share of 400 q/s is one sampler send every 250 ms (a sample_infohashes to a due
    // node, or a find_node growing the pool when none is due).
    let sampler =
        |st: &magnetar_core::dht::EngineStats| st.sent_sample_infohashes + st.sent_find_node;
    let before = sampler(&d.stats());
    d.set_sample_share(0.01);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let n = sampler(&d.stats()) - before;
    assert!((2..=8).contains(&n), "{n}");
    d.set_sample_share(1.0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sampler(&d.stats()) - before > n + 20);
    d.shutdown().await;
    for (d, _) in net {
        d.shutdown().await;
    }
}
