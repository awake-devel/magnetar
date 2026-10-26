//! Live liveness checks against real trackers and the mainline DHT, using real infohashes from
//! `docs/research/fixtures/kind2003_sample.jsonl`. Run with `cargo test --test liveness_live -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use magnetar_core::liveness::{self, LivenessConfig, dht, http, udp};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/research/fixtures/kind2003_sample.jsonl"
);

/// (infohash hex, trackers) for fixture events with a clean 40-hex `x` tag.
fn fixture() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in std::fs::read_to_string(FIXTURE).unwrap().lines() {
        let ev: serde_json::Value = serde_json::from_str(line).unwrap();
        let tags = ev["tags"].as_array().unwrap();
        let tag = |name: &str| -> Vec<String> {
            tags.iter()
                .filter(|t| t[0] == name)
                .filter_map(|t| t[1].as_str().map(str::to_string))
                .collect()
        };
        let Some(x) = tag("x")
            .into_iter()
            .map(|x| x.to_ascii_lowercase())
            .find(|x| x.len() == 40 && x.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        if !out.iter().any(|(h, _)| *h == x) {
            out.push((x, tag("tracker")));
        }
    }
    out
}

fn raw(hex_hash: &str) -> [u8; 20] {
    let mut h = [0u8; 20];
    hex::decode_to_slice(hex_hash, &mut h).unwrap();
    h
}

async fn udp_live(tracker: &str) {
    let items = fixture();
    let hashes: Vec<[u8; 20]> = items.iter().map(|(h, _)| raw(h)).collect();
    let start = Instant::now();
    let res = udp::scrape(tracker, &hashes, Duration::from_secs(3))
        .await
        .unwrap();
    let answered = res.iter().flatten().count();
    let alive = res.iter().flatten().filter(|s| s.seeders > 0).count();
    let top = res.iter().flatten().map(|s| s.seeders).max();
    println!(
        "{tracker}: {} hashes, {answered} answered, {alive} with seeders, max seeders {top:?}, {:?}",
        hashes.len(),
        start.elapsed()
    );
    assert_eq!(res.len(), hashes.len());
    assert_eq!(answered, hashes.len(), "≤74 per packet is never truncated");
}

#[tokio::test]
#[ignore = "network"]
async fn live_udp_opentrackr() {
    udp_live("udp://tracker.opentrackr.org:1337/announce").await;
}

#[tokio::test]
#[ignore = "network"]
async fn live_udp_torrent_eu_org() {
    udp_live("udp://tracker.torrent.eu.org:451/announce").await;
}

#[tokio::test]
#[ignore = "network"]
async fn live_http_nyaa() {
    let tracker = "http://nyaa.tracker.wf:7777/announce";
    let items: Vec<_> = fixture()
        .into_iter()
        .filter(|(_, t)| t.iter().any(|t| t == tracker))
        .collect();
    assert!(!items.is_empty());
    let hashes: Vec<[u8; 20]> = items.iter().map(|(h, _)| raw(h)).collect();
    let client = magnetar_core::http::client(Duration::from_secs(10)).unwrap();
    let start = Instant::now();
    let res = http::scrape(&client, tracker, &hashes).await.unwrap();
    for ((h, _), s) in items.iter().zip(&res) {
        println!("  {h} {s:?}");
    }
    let alive = res.iter().flatten().filter(|s| s.seeders > 0).count();
    println!(
        "{tracker}: {} hashes, {alive} with seeders, {:?}",
        hashes.len(),
        start.elapsed()
    );
    assert!(res.iter().all(Option::is_some));
}

#[tokio::test]
#[ignore = "network"]
async fn live_dht_sintel() {
    let sintel = raw("08ada5a7a6183aae1e09d831df6748d566095a10");
    let start = Instant::now();
    let r = dht::scrape(&sintel, Duration::from_secs(15)).await.unwrap();
    println!("DHT Sintel: {r:?} in {:?}", start.elapsed());
    assert!(r.nodes_queried > 0);
    assert!(
        r.total_est > 0 || r.values_ips > 0,
        "Sintel always has a swarm"
    );
}

#[tokio::test]
#[ignore = "network"]
async fn live_check_batch_fixture() {
    let items = fixture();
    let cfg = LivenessConfig {
        use_dht: true,
        dht_budget: Duration::from_secs(10),
        ..Default::default()
    };
    let start = Instant::now();
    let out = liveness::check_batch(&items, &cfg, magnetar_core::now()).await;
    let elapsed = start.elapsed();
    let mut by_source = std::collections::BTreeMap::new();
    for l in &out {
        *by_source.entry(format!("{:?}", l.source)).or_insert(0) += 1;
        println!(
            "  {} {:?} s={:?} l={:?} c={:?} dht={:?} ok={} failed={}",
            l.infohash,
            l.source,
            l.seeders,
            l.leechers,
            l.completed,
            l.dht_peers_est,
            l.trackers_ok,
            l.trackers_failed
        );
    }
    let alive = out.iter().filter(|l| l.best_peers() > Some(0)).count();
    let dead = out.iter().filter(|l| l.best_peers() == Some(0)).count();
    println!(
        "check_batch: {} items, alive {alive}, dead {dead}, by source {by_source:?}, {elapsed:?}",
        out.len()
    );
    assert_eq!(out.len(), items.len());
}

/// DHT walks one after another through a shared node cache (D45): later walks start next to
/// the target. Prints the time and answering nodes per walk.
#[tokio::test]
#[ignore = "network"]
async fn dht_walks_with_a_node_cache() {
    let cache = dht::NodeCache::new(liveness::DHT_NODE_CACHE);
    let mut total = Duration::ZERO;
    let items = fixture();
    for (ih, _) in items.iter().take(12) {
        let start = Instant::now();
        let r = dht::scrape_cached(&cache, &raw(ih), Duration::from_secs(12)).await;
        let took = start.elapsed();
        total += took;
        match r {
            Ok(d) => println!(
                "{ih}: {took:?}, nodes {}, bf {}, values {}, est {}, ignored {}, cache {}",
                d.nodes_queried,
                d.bf_responses,
                d.values_ips,
                d.total_est,
                d.ignored_answers,
                cache.len()
            ),
            Err(e) => println!("{ih}: {took:?}, {e}"),
        }
    }
    println!("12 walks in {total:?}");
}
