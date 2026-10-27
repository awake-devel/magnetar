//! Crawl map (D101–D105, docs/CRAWLMAP.md) against a scratch schema: DHT counts by country and
//! network from triage to stored torrents, relay locations, the per-route counters, the two
//! endpoints, the worker protocol fields — and the privacy rule (D101): no IP address in any
//! column, stored value or API answer. Addresses come from the documentation ranges
//! (RFC 5737), resolved by a fixed table instead of the DB-IP files.
//! Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

use crate::common;
use crate::dht_fetch_pg::{info, sha1};

use std::net::SocketAddrV4;
use std::sync::Arc;

use common::*;
use magnetar_core::api::{CrawlMap as MapAnswer, CrawlSources, Health};
use magnetar_core::dht::Via;
use magnetar_core::metainfo::outcome_from_raw;
use magnetar_core::store::Store;
use magnetar_server::crawlmap::{CrawlMap, Layer};
use magnetar_server::dht::persist::{FetchResult, persist_batch};
use magnetar_server::dht::{screen, store as dstore};
use magnetar_server::fleet::proto::{AsName, GeoTally, TriageItem, TriageRequest};
use magnetar_server::geo::{GeoCode, GeoMode, TableResolver};
use magnetar_server::{AppState, PgStore};
use serde_json::Value as J;

const NZ_NET: &str = "203.0.113.0";
const FR_NET: &str = "198.51.100.0";

fn table() -> TableResolver {
    TableResolver {
        rows: vec![
            (
                NZ_NET.parse().unwrap(),
                24,
                GeoCode::from_parts(Some("NZ"), Some(64500)),
                "Example Aotearoa Ltd",
            ),
            (
                FR_NET.parse().unwrap(),
                24,
                GeoCode::from_parts(Some("FR"), Some(64501)),
                "Exemple SAS",
            ),
            // In the ASN table but without a country.
            (
                "192.0.2.0".parse().unwrap(),
                24,
                GeoCode::from_parts(None, Some(64502)),
                "Anycast Example",
            ),
        ],
    }
}

fn map() -> Arc<CrawlMap> {
    let m = Arc::new(CrawlMap::new(GeoMode::On, 16));
    m.set_resolver(Arc::new(table()));
    m
}

fn addr(s: &str) -> Option<SocketAddrV4> {
    Some(s.parse().unwrap())
}

/// True when `s` contains something shaped like an IPv4 address (four dot-separated numbers
/// 0–255) or an IPv6 address (≥ 2 colons between hex groups, not a URL scheme).
fn looks_like_ip(s: &str) -> bool {
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let rest: String = bytes[i..].iter().collect();
            let cand: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts: Vec<&str> = cand.split('.').collect();
            if parts.len() >= 4
                && parts[..4].iter().all(|p| {
                    !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|n| n <= 255)
                })
            {
                return true;
            }
        }
        i += 1;
    }
    s.split(|c: char| !(c.is_ascii_hexdigit() || c == ':'))
        .any(|t| t.matches(':').count() >= 2 && t.chars().any(|c| c.is_ascii_hexdigit()))
}

/// Every string and key of a JSON answer: no IP-looking value, no address-named field.
fn assert_no_ip(v: &J, path: &str) {
    match v {
        J::String(s) => assert!(!looks_like_ip(s), "IP-looking string at {path}: {s:?}"),
        J::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, x)| assert_no_ip(x, &format!("{path}[{i}]"))),
        J::Object(o) => {
            for (k, x) in o {
                let lk = k.to_ascii_lowercase();
                assert!(
                    !(lk == "ip"
                        || lk.contains("addr")
                        || lk.ends_with("_ip")
                        || lk.contains("ip_")),
                    "address-like field {path}.{k}"
                );
                assert_no_ip(x, &format!("{path}.{k}"));
            }
        }
        _ => {}
    }
}

#[test]
fn ip_detector_works() {
    assert!(looks_like_ip("203.0.113.9"));
    assert!(looks_like_ip("wss://198.51.100.1:443/"));
    assert!(looks_like_ip("2001:db8::1"));
    assert!(!looks_like_ip("wss://relay.example.com/"));
    assert!(!looks_like_ip("v1.2.3"));
    assert!(!looks_like_ip(
        "IP Geolocation by DB-IP (https://db-ip.com), CC BY 4.0"
    ));
    assert!(!looks_like_ip("NZ"));
}

/// The schema has no address-typed column, and the crawl map's columns are codes only.
#[tokio::test]
async fn schema_has_no_address_columns() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let inet: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
         WHERE table_schema = current_schema() AND data_type IN ('inet', 'cidr', 'macaddr')",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(inet.is_empty(), "address-typed columns: {inet:?}");
    let cols: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name IN ('geo_counts_hourly', 'geo_counts_total', 'geo_as_names')
         ORDER BY 1, ordinal_position",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let names: Vec<String> = cols.iter().map(|(t, c)| format!("{t}.{c}")).collect();
    assert_eq!(
        names,
        [
            "geo_as_names.asn",
            "geo_as_names.org",
            "geo_as_names.updated_at",
            "geo_counts_hourly.layer",
            "geo_counts_hourly.metric",
            "geo_counts_hourly.bucket",
            "geo_counts_hourly.cc",
            "geo_counts_hourly.asn",
            "geo_counts_hourly.value",
            "geo_counts_total.layer",
            "geo_counts_total.metric",
            "geo_counts_total.cc",
            "geo_counts_total.asn",
            "geo_counts_total.value",
        ]
    );
    for (t, c) in [
        ("dht_seen", "src_cc"),
        ("dht_seen", "src_asn"),
        ("relays", "geo_cc"),
        ("relays", "geo_asn"),
        ("relays", "torrents_found"),
    ] {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2",
        )
        .bind(t)
        .bind(c)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "{t}.{c}");
    }
}

/// Every text value the crawl map wrote: codes and AS names, never an address.
async fn assert_stored_values_clean(pool: &sqlx::PgPool) {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT layer || ' ' || metric || ' ' || cc FROM geo_counts_hourly
         UNION ALL SELECT layer || ' ' || metric || ' ' || cc FROM geo_counts_total
         UNION ALL SELECT org FROM geo_as_names
         UNION ALL SELECT COALESCE(src_cc, '') FROM dht_seen
         UNION ALL SELECT COALESCE(geo_cc, '') FROM relays",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(!texts.is_empty());
    for t in texts {
        assert!(!looks_like_ip(&t), "stored IP-looking value {t:?}");
    }
    let ccs: Vec<String> = sqlx::query_scalar(
        "SELECT cc FROM geo_counts_total UNION SELECT src_cc FROM dht_seen WHERE src_cc IS NOT NULL
         UNION SELECT geo_cc FROM relays WHERE geo_cc IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    for c in ccs {
        assert!(
            c.is_empty() || (c.len() == 2 && c.chars().all(|x| x.is_ascii_uppercase())),
            "{c:?}"
        );
    }
}

fn verified(raw: &[u8], via: Via) -> FetchResult {
    let h = sha1(raw);
    FetchResult {
        sighted: h,
        via,
        peers_seen: 2,
        result: Ok(outcome_from_raw(h, raw, 1 << 20).expect("valid dict")),
    }
}

#[tokio::test]
async fn dht_counts_by_country_and_network() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let m = map();
    let now = magnetar_core::now();

    // Three hashes: two from a NZ node (sampled), one from a FR querier (get_peers).
    let raws: Vec<Vec<u8>> = [
        "Map.Test.One.2024",
        "Map.Test.Two.2024",
        "Map.Test.Three.2024",
    ]
    .iter()
    .map(|n| info(n, "0"))
    .collect();
    let hs: Vec<[u8; 20]> = raws.iter().map(|r| sha1(r)).collect();
    let from = [
        addr("203.0.113.9:6881"),
        addr("203.0.113.9:6881"),
        addr("198.51.100.4:51413"),
    ];
    for f in from {
        m.dht_node(f, now);
    }
    let geo = m.dht_hashes(from, true);
    let batch = vec![
        (hs[0], Via::Sample),
        (hs[1], Via::Sample),
        (hs[2], Via::GetPeers),
    ];
    let o = dstore::triage_geo(pool, &batch, &geo, now, false)
        .await
        .unwrap();
    assert_eq!(o.queued.len(), 3);
    let src: Vec<(Option<String>, Option<i32>)> =
        sqlx::query_as("SELECT src_cc, src_asn FROM dht_seen ORDER BY src_cc")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        src,
        vec![
            (Some("FR".into()), Some(64501)),
            (Some("NZ".into()), Some(64500)),
            (Some("NZ".into()), Some(64500)),
        ]
    );

    // Two of them are stored: the stored count follows the codes kept in dht_seen.
    let screen = screen::global_screen().expect("test blocklist installed");
    let p = persist_batch(
        pool,
        &[
            verified(&raws[0], Via::Sample),
            verified(&raws[2], Via::GetPeers),
        ],
        Some(screen.as_ref()),
        false,
        now,
    )
    .await
    .unwrap();
    assert_eq!(p.stored, 2);
    assert_eq!(p.stored_geo.len(), 2);
    m.add_stored(&p.stored_geo);
    m.flush(pool, now).await.unwrap();

    let a = m.map(pool, Layer::Dht, true, now).await.unwrap();
    assert!(a.available);
    assert_eq!(a.metrics, ["hashes", "stored", "nodes"]);
    let nz = a
        .countries
        .iter()
        .find(|c| c.country.as_deref() == Some("NZ"))
        .unwrap();
    assert_eq!(nz.counts, vec![2, 1, 1]);
    let fr = a
        .countries
        .iter()
        .find(|c| c.country.as_deref() == Some("FR"))
        .unwrap();
    assert_eq!(fr.counts, vec![1, 1, 1]);
    assert_eq!(
        a.countries[0].country.as_deref(),
        Some("NZ"),
        "ordered by hashes"
    );
    assert_eq!(a.totals, vec![3, 2, 2]);
    assert_eq!(a.unknown, vec![0, 0, 0]);
    let net = a.networks.iter().find(|n| n.asn == Some(64500)).unwrap();
    assert_eq!(net.as_org.as_deref(), Some("Example Aotearoa Ltd"));
    assert_eq!(net.country.as_deref(), Some("NZ"));
    assert!(a.attribution.contains("DB-IP"));
    let all = m.map(pool, Layer::Dht, false, now + 120).await.unwrap();
    assert_eq!(all.totals, vec![3, 2, 2]);

    // A second flush adds up; a repeat node the same day is not counted again.
    m.dht_node(addr("203.0.113.9:7000"), now);
    m.dht_hashes([addr("192.0.2.1:1"), None], true);
    m.flush(pool, now).await.unwrap();
    let total: i64 = sqlx::query_scalar(
        "SELECT sum(value)::int8 FROM geo_counts_total WHERE layer = 'dht' AND metric = 'hashes'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(total, 5);
    let nodes: i64 =
        sqlx::query_scalar("SELECT sum(value)::int8 FROM geo_counts_total WHERE metric = 'nodes'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(nodes, 2);

    // Per-route counters (stats_series): triaged and stored, by route.
    let series: Vec<(String, i64)> = sqlx::query_as(
        "SELECT metric, sum(value)::int8 FROM stats_series
         WHERE metric LIKE 'dht_seen_%' OR metric LIKE 'dht_stored_%' GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        series,
        vec![
            ("dht_seen_get_peers".into(), 1),
            ("dht_seen_sample".into(), 2),
            ("dht_stored_get_peers".into(), 1),
            ("dht_stored_sample".into(), 1),
        ]
    );
    let s = m.sources(pool, Vec::new(), now).await.unwrap();
    let r = |v: &str| s.dht.routes.iter().find(|x| x.via == v).cloned().unwrap();
    assert_eq!((r("sample").seen, r("sample").stored), (2, 1));
    assert_eq!((r("get_peers").seen, r("get_peers").stored), (1, 1));
    assert_eq!((r("announce").seen, r("announce").stored), (0, 0));
    assert_eq!(s.dht.hourly.len(), 48);
    let last = s.dht.hourly.last().unwrap();
    assert_eq!((last.seen.sample, last.stored.get_peers), (2, 1));

    for v in [
        serde_json::to_value(&a).unwrap(),
        serde_json::to_value(&s).unwrap(),
    ] {
        assert_no_ip(&v, "$");
    }
    assert_stored_values_clean(pool).await;
}

#[tokio::test]
async fn without_a_database_nothing_is_counted() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let m = CrawlMap::new(GeoMode::On, 16);
    let now = magnetar_core::now();
    m.dht_node(addr("203.0.113.9:6881"), now);
    let geo = m.dht_hashes([addr("203.0.113.9:6881")], true);
    assert_eq!(geo, vec![None]);
    let h = sha1(&info("No.Geo.2024", "0"));
    dstore::triage_geo(pool, &[(h, Via::Sample)], &geo, now, false)
        .await
        .unwrap();
    let src: Option<String> = sqlx::query_scalar("SELECT src_cc FROM dht_seen")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(src, None, "not resolved: NULL, not unknown");
    assert_eq!(m.flush(pool, now).await.unwrap(), 0);
    let a = m.map(pool, Layer::Dht, true, now).await.unwrap();
    assert!(!a.available);
    assert_eq!(a.reason.as_deref(), Some("missing"));
    assert!(a.countries.is_empty());
    let off = CrawlMap::new(GeoMode::Off, 16);
    off.set_resolver(Arc::new(table()));
    assert!(off.resolver().is_none(), "off stays off");
    let a = off.map(pool, Layer::Nostr, false, now).await.unwrap();
    assert_eq!(a.reason.as_deref(), Some("disabled"));
}

#[tokio::test]
async fn relays_are_located_without_keeping_addresses() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let m = map();
    let now = magnetar_core::now();
    for (url, src) in [
        ("wss://203.0.113.5/", "seed"),
        ("wss://198.51.100.20:7447/", "nip65"),
        ("wss://relay.invalid/", "hint"),
        ("ws://example.onion/", "nip66"),
    ] {
        s.upsert_relay(url, src).await.unwrap();
    }
    s.record_relay_result("wss://203.0.113.5/", true, None, 50, now)
        .await
        .unwrap();
    s.record_relay_torrents("wss://203.0.113.5/", 40)
        .await
        .unwrap();
    s.record_relay_result("wss://198.51.100.20:7447/", true, None, 5, now)
        .await
        .unwrap();
    m.snapshot_relays(pool).await.unwrap(); // baseline
    assert_eq!(m.resolve_relays(pool, now).await.unwrap(), 4);
    let geo: Vec<(String, Option<String>, Option<i32>)> =
        sqlx::query_as("SELECT url, geo_cc, geo_asn FROM relays ORDER BY url")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        geo,
        vec![
            ("ws://example.onion/".into(), Some("".into()), Some(0)),
            (
                "wss://198.51.100.20:7447/".into(),
                Some("FR".into()),
                Some(64501)
            ),
            ("wss://203.0.113.5/".into(), Some("NZ".into()), Some(64500)),
            ("wss://relay.invalid/".into(), Some("".into()), Some(0)),
        ]
    );
    // Delivered since the baseline: counted in the day window by where the relay is.
    s.record_relay_result("wss://203.0.113.5/", true, None, 7, now)
        .await
        .unwrap();
    s.record_relay_torrents("wss://203.0.113.5/", 3)
        .await
        .unwrap();
    m.snapshot_relays(pool).await.unwrap();
    m.flush(pool, now).await.unwrap();
    let day = m.map(pool, Layer::Nostr, true, now).await.unwrap();
    assert_eq!(day.metrics, ["relays", "events", "torrents"]);
    let nz = day
        .countries
        .iter()
        .find(|c| c.country.as_deref() == Some("NZ"))
        .unwrap();
    assert_eq!(nz.counts, vec![1, 7, 3]);
    let all = m.map(pool, Layer::Nostr, false, now).await.unwrap();
    let nz = all
        .countries
        .iter()
        .find(|c| c.country.as_deref() == Some("NZ"))
        .unwrap();
    assert_eq!(nz.counts, vec![1, 57, 43]);
    assert_eq!(all.unknown[0], 2, "onion and unresolvable relays");

    let src = m.sources(pool, Vec::new(), now).await.unwrap();
    let seed = src
        .nostr
        .by_source
        .iter()
        .find(|x| x.source == "seed")
        .unwrap();
    assert_eq!(
        (seed.relays, seed.delivered, seed.events, seed.torrents),
        (1, 1, 57, 43)
    );
    let top = &src.nostr.top_relays[0];
    assert_eq!(
        top.url, "wss://(IP address)",
        "IP-literal relay URLs are not shown"
    );
    assert_eq!((top.country.as_deref(), top.asn), (Some("NZ"), Some(64500)));
    assert_eq!(top.as_org.as_deref(), Some("Example Aotearoa Ltd"));
    for v in [
        serde_json::to_value(&all).unwrap(),
        serde_json::to_value(&src).unwrap(),
    ] {
        assert_no_ip(&v, "$");
    }
    assert_stored_values_clean(pool).await;
}

async fn spawn(store: &PgStore, m: Arc<CrawlMap>) -> String {
    let state = AppState::new(Arc::new(store.clone()), TOKEN).with_crawlmap(m);
    let app = magnetar_server::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn endpoints_and_feature() {
    let Some(db) = test_db().await else { return };
    let m = map();
    let base = spawn(&db.store, m.clone()).await;
    let http = reqwest::Client::new();
    let h: Health = http
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(h.features.iter().any(|f| f == "crawl_map"));
    assert!(h.features.iter().any(|f| f == "crawl_sources"));
    let get = |p: &str| http.get(format!("{base}{p}")).bearer_auth(TOKEN).send();
    let r = get("/api/v1/stats/sources").await.unwrap();
    assert_eq!(r.status(), 200);
    let v: J = r.json().await.unwrap();
    assert_no_ip(&v, "$");
    let s: CrawlSources = serde_json::from_value(v).unwrap();
    assert_eq!(s.machines[0].name, "server");
    assert_eq!(s.dht.routes.len(), 3);
    for q in ["layer=dht&window=day", "layer=nostr&window=all", ""] {
        let r = get(&format!("/api/v1/stats/map?{q}")).await.unwrap();
        assert_eq!(r.status(), 200, "{q}");
        let v: J = r.json().await.unwrap();
        assert_no_ip(&v, "$");
        let a: MapAnswer = serde_json::from_value(v).unwrap();
        assert!(a.available);
    }
    assert_eq!(
        get("/api/v1/stats/map?layer=ip").await.unwrap().status(),
        400
    );
    let r = http
        .get(format!("{base}/api/v1/stats/map"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

/// Worker triage items with codes are counted and kept as codes; items without (an older
/// worker, or no database there) are triaged as before.
#[tokio::test]
async fn worker_triage_codes() {
    use magnetar_server::fleet::{Fleet, FleetConfig, parse_tokens};
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    const WT: &str = "wg-token-cccccccccccccccccccccccc";
    let fleet = Fleet::new(
        FleetConfig {
            tokens: parse_tokens(&format!("wg:{WT}"), Some(TOKEN)).unwrap(),
            lease_secs: 900,
            max_lease: 100,
            metadata_max: 1 << 20,
            fetch_known: false,
        },
        screen::global_screen(),
    );
    let m = Arc::new(CrawlMap::new(GeoMode::On, 16));
    let state = AppState::new(Arc::new(db.store.clone()), TOKEN)
        .with_fleet(Arc::new(fleet))
        .with_crawlmap(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap()
    });
    let c = magnetar_server::worker::client::Client::new(&base, WT).unwrap();
    let h1 = sha1(&info("Worker.Geo.One", "0"));
    let h2 = sha1(&info("Worker.Geo.Two", "0"));
    let h3 = sha1(&info("Worker.Geo.Three", "0"));
    let req = TriageRequest {
        items: vec![
            TriageItem {
                h: hex::encode(h1),
                via: "sample".into(),
                cc: Some("DE".into()),
                asn: Some(64510),
            },
            TriageItem {
                h: hex::encode(h2),
                via: "announce".into(),
                cc: Some("".into()),
                asn: None,
            },
            TriageItem {
                h: hex::encode(h3),
                via: "get_peers".into(),
                ..Default::default()
            },
        ],
        geo_nodes: vec![GeoTally {
            cc: Some("DE".into()),
            asn: Some(64510),
            n: 4,
        }],
        as_names: vec![AsName {
            asn: 64510,
            org: "Beispiel GmbH".into(),
        }],
    };
    // The wire form: codes only.
    let wire = serde_json::to_value(&req).unwrap();
    assert_no_ip(&wire, "$");
    let r: magnetar_server::fleet::proto::TriageResponse = c
        .post(magnetar_server::fleet::proto::TRIAGE, &req)
        .await
        .unwrap();
    assert_eq!(r.fetch.len(), 3);
    let src: Vec<(Vec<u8>, Option<String>, Option<i32>)> =
        sqlx::query_as("SELECT infohash, src_cc, src_asn FROM dht_seen")
            .fetch_all(pool)
            .await
            .unwrap();
    let of = |h: [u8; 20]| {
        src.iter()
            .find(|r| r.0 == h)
            .map(|r| (r.1.clone(), r.2))
            .unwrap()
    };
    assert_eq!(of(h1), (Some("DE".into()), Some(64510)));
    assert_eq!(of(h2), (Some("".into()), Some(0)));
    assert_eq!(of(h3), (None, None));
    m.flush(pool, magnetar_core::now()).await.unwrap();
    let rows: Vec<(String, String, i32, i64)> =
        sqlx::query_as("SELECT metric, cc, asn, value FROM geo_counts_total ORDER BY 1, 2")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("hashes".into(), "".into(), 0, 1),
            ("hashes".into(), "DE".into(), 64510, 1),
            ("nodes".into(), "DE".into(), 64510, 4),
        ]
    );
    let org: String = sqlx::query_scalar("SELECT org FROM geo_as_names WHERE asn = 64510")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(org, "Beispiel GmbH");
    // An older worker's JSON (no geo fields) still parses.
    let old: TriageRequest =
        serde_json::from_str(r#"{"items":[{"h":"00","via":"sample"}]}"#).unwrap();
    assert!(old.geo_nodes.is_empty() && old.items[0].cc.is_none());
    let machines =
        magnetar_server::api::crawl_machines(None, &Default::default(), &Fleet::default(), 0);
    assert_eq!(machines.len(), 1);
}
