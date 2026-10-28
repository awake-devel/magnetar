//! Engine throughput bench on loopback (D106, docs/DHT.md §20): the crawler's own sockets,
//! pacers, transaction maps, pool and lookups against a fake DHT on loopback, with no network
//! and no metadata pipeline in the way: 1 000 fake nodes answer at once; `DHT_BENCH_GHOSTS` ‰ of
//! the answers (default 250) also name an address that never answers. Ignored by default; run in release:
//!
//! ```sh
//! DHT_BENCH_NODES=8 DHT_BENCH_QPS=20000 DHT_BENCH_LOOKUPS=4096 DHT_BENCH_SECS=20 \
//!   cargo test --release -p magnetar-core --test it dht_bench:: -- --ignored --nocapture
//! ```
//!
//! Prints the sent rate against the cap and the CPU of the crawler's threads (the fake DHT runs
//! on its own runtime, threads named `fake-dht`, and is reported apart).
#![cfg(feature = "dht-crawl")]

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use magnetar_core::dht::id::distance;
use magnetar_core::dht::krpc::{self, Body, CompactNode, Message, Query, Response};
use magnetar_core::dht::{Dht, EngineConfig, Mode, collect_peers};
use tokio::net::UdpSocket;

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// CPU seconds per thread of this process (utime + stime): tid → (is fake DHT, seconds).
fn cpu_by_thread() -> HashMap<String, (bool, f64)> {
    let hz = 100.0; // CLK_TCK on Linux
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return out;
    };
    for t in dir.flatten() {
        let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let fake = stat[open + 1..close].starts_with("fake-dht");
        let f: Vec<&str> = stat[close + 2..].split(' ').collect();
        let ticks = f.get(11).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0)
            + f.get(12).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
        out.insert(
            t.file_name().to_string_lossy().into_owned(),
            (fake, ticks / hz),
        );
    }
    out
}

/// A fake DHT node table: 1 000 addresses on 127.0.9.1–127.0.12.250 with fixed random ids.
fn fake_table() -> Vec<CompactNode> {
    (9..=12u8)
        .flat_map(|c| (1..=250u8).map(move |d| (c, d)))
        .map(|(c, d)| CompactNode {
            id: rand::random(),
            addr: SocketAddrV4::new(Ipv4Addr::new(127, 0, c, d), 16_000),
        })
        .collect()
}

async fn serve(
    sock: UdpSocket,
    me: CompactNode,
    table: Arc<Vec<CompactNode>>,
    ghosts: u32,
    stop: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let Ok(Ok((n, SocketAddr::V4(src)))) =
            tokio::time::timeout(Duration::from_millis(200), sock.recv_from(&mut buf)).await
        else {
            continue;
        };
        let Ok(msg) = krpc::decode(&buf[..n]) else {
            continue;
        };
        let Body::Query { query, .. } = msg.body else {
            continue;
        };
        let target = match query {
            Query::GetPeers { info_hash } => info_hash,
            Query::FindNode { target } => target,
            Query::Ping => me.id,
            _ => continue,
        };
        let mut near: Vec<([u8; 20], CompactNode)> = table
            .iter()
            .filter(|c| c.addr != me.addr)
            .map(|c| (distance(&c.id, &target), *c))
            .collect();
        near.select_nth_unstable_by_key(8, |x| x.0);
        // Eight real nodes, and in `ghosts` ‰ of the answers one of them is replaced by an
        // address that never answers: the crawler's pool grows large and lookups meet timeouts.
        let mut near: Vec<CompactNode> = near[..8].iter().map(|x| x.1).collect();
        if rand::random::<u32>() % 1000 < ghosts {
            let [a, b, c]: [u8; 3] = rand::random();
            near[7] = CompactNode {
                id: rand::random(),
                addr: SocketAddrV4::new(Ipv4Addr::new(127, 20 + a % 40, b, c.max(1)), 6881),
            };
        }
        // Nodes close to the hash know a few peers.
        let values = if distance(&me.id, &target)[0] < 4 {
            (1..=6u8)
                .map(|k| SocketAddrV4::new(Ipv4Addr::new(127, 0, 10, k), 6881))
                .collect()
        } else {
            Vec::new()
        };
        let wire = krpc::encode(&Message {
            tid: msg.tid,
            body: Body::Response(Response {
                id: me.id,
                nodes: near,
                values,
                token: Some(vec![1, 2, 3, 4]),
                ..Default::default()
            }),
            ip: Some(src),
            version: None,
        });
        let _ = sock.send_to(&wire, SocketAddr::V4(src)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "bench: run in release with --ignored --nocapture"]
async fn engine_throughput() {
    let nodes = env("DHT_BENCH_NODES", 4) as usize;
    let qps = env("DHT_BENCH_QPS", 5000) as u32;
    let lookups = env("DHT_BENCH_LOOKUPS", 2048) as usize;
    let secs = env("DHT_BENCH_SECS", 15);
    let ghosts = env("DHT_BENCH_GHOSTS", 250) as u32;

    let table = Arc::new(fake_table());
    let stop = Arc::new(AtomicBool::new(false));
    let fake = {
        let table = table.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(6)
                .thread_name("fake-dht")
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let mut tasks = Vec::new();
                for me in table.iter().copied() {
                    let sock = UdpSocket::bind(me.addr).await.expect("bind fake node");
                    tasks.push(tokio::spawn(serve(
                        sock,
                        me,
                        table.clone(),
                        ghosts,
                        stop.clone(),
                    )));
                }
                for t in tasks {
                    let _ = t.await;
                }
            });
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut cfg = EngineConfig::new(Mode::Aggressive);
    cfg.bind = Ipv4Addr::new(127, 0, 8, 1);
    cfg.ports = vec![0; nodes];
    cfg.qps = qps;
    cfg.pool_cap = 1_000_000;
    cfg.bootstrap = table.iter().take(16).map(|c| c.addr).collect();
    cfg.allow_loopback = true;
    cfg.sample = false;
    cfg.query_timeout = Duration::from_millis(1000);
    cfg.lookup_timeout = Duration::from_millis(1000);
    // 250 fake addresses stand for the whole DHT: the per-IP rule would cap the bench at 250 q/s.
    cfg.per_ip_qps = 1_000_000.0;
    let (dht, _rx) = Dht::start(cfg).await.expect("start crawler");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let cpu0 = cpu_by_thread();
    let s0 = dht.stats();
    let t0 = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..lookups {
        let d = dht.clone();
        set.spawn(async move {
            let mut n = 0u64;
            while tokio::time::Instant::now() < deadline {
                let h: [u8; 20] = rand::random();
                let c = collect_peers(&d, h, None, None, Duration::from_secs(10), 24).await;
                n += u64::from(!c.peers.is_empty());
            }
            n
        });
    }
    let mut found = 0;
    while let Some(r) = set.join_next().await {
        found += r.unwrap_or(0);
    }
    let el = t0.elapsed().as_secs_f64();
    let s1 = dht.stats();
    let cpu1 = cpu_by_thread();
    stop.store(true, Ordering::Relaxed);
    dht.shutdown().await;
    let _ = fake.join();

    let sent = (s1.queries_sent - s0.queries_sent) as f64 / el;
    let answered = (s1.answered - s0.answered) as f64 / el;
    let looks = (s1.lookups - s0.lookups) as f64 / el;
    let mut crawler = 0.0;
    let mut crawler_max = 0.0f64;
    let mut fake_cpu = 0.0;
    for (tid, (fake, v)) in &cpu1 {
        let d = v - cpu0.get(tid).map_or(0.0, |x| x.1);
        if *fake {
            fake_cpu += d;
        } else {
            crawler += d;
            crawler_max = crawler_max.max(d);
        }
    }
    println!(
        "BENCH nodes={nodes} qps={qps} lookups={lookups} ghosts={ghosts}: sent {sent:.0} q/s ({:.0} % of cap), answered {answered:.0}/s, \
         lookups {looks:.0}/s with peers {found}, rate_limited {}, busy {}, crawler CPU {:.2} cores \
         (busiest thread ≤ {:.2}), fake DHT {:.2} cores",
        100.0 * sent / f64::from(qps),
        s1.rate_limited - s0.rate_limited,
        s1.busy - s0.busy,
        crawler / el,
        crawler_max / el,
        fake_cpu / el,
    );
}
