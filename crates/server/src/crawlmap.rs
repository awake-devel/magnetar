//! Crawl map (D101–D105, docs/CRAWLMAP.md): where discoveries come from (DHT routes, relay
//! sources, machines) and which countries and networks (AS numbers) lead to them.
//!
//! **Privacy rule (D101, user decision 2026-10-27).** No IP address is ever stored, logged,
//! sent to the app or written to disk — not raw, not hashed. An address is turned into a
//! [`GeoCode`] (country, AS number) here, in memory, at the moment it is counted, and dropped.
//! Only aggregate counters by code are kept ([`Tally`] → `geo_counts_*`). The "nodes heard
//! from" counter dedupes in memory with a Bloom filter keyed by a random per-day SipHash key
//! (never written, replaced at each UTC day), so not even a keyed hash of an address outlives
//! the day or leaves the process. Relay hosts get the same treatment: their resolved address
//! becomes `relays.geo_cc` / `geo_asn` and is dropped.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures::StreamExt;
use magnetar_core::api::{
    CrawlMap as CrawlMapAnswer, CrawlSources, DhtRouteCount, DhtRouteHour, DhtSources, GeoCount,
    MachineSources, NostrSources, RelaySourceCount, RelayYield,
};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::daemons::stop_requested;
use crate::geo::{self, GeoCode, GeoConfig, GeoMode, Resolver, Unavailable};

/// How often the in-memory tally is written (and machine rates sampled).
pub const FLUSH_EVERY: Duration = Duration::from_secs(60);
/// Hourly rows older than this are deleted (the window is 24 h).
pub const HOURLY_KEEP_SECS: i64 = 49 * 3600;
/// Geo database refresh check.
pub const REFRESH_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Relay hosts are resolved again after this, a batch per [`RELAY_EVERY`].
pub const RELAY_RECHECK_SECS: i64 = 24 * 3600;
const RELAY_EVERY: Duration = Duration::from_secs(600);
const RELAY_BATCH: i64 = 300;
const RELAY_SNAPSHOT_EVERY: Duration = Duration::from_secs(600);
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
/// Answers are cached this long.
const MAP_TTL: Duration = Duration::from_secs(60);
const SOURCES_TTL: Duration = Duration::from_secs(60);
/// The whole-table route counts (`dht_seen` has millions of rows) are refreshed less often.
const ROUTES_TTL: Duration = Duration::from_secs(600);
const ROUTES_TIMEOUT_MS: u32 = 20_000;
pub const TOP_NETWORKS: usize = 50;
pub const TOP_RELAYS: i64 = 20;
/// AS names already written or queued; cleared beyond this (names are then written again).
const NAMED_MAX: usize = 200_000;
/// Worker uploads: most tally rows / names accepted per triage request.
pub const MAX_UPLOAD_ROWS: usize = 20_000;

/// Bloom filter sizes (bits, log2): the server sees ~1–2 M DHT nodes a day (2 MiB, ~3 %
/// false "already seen" at 2 M), a worker fewer (512 KiB).
pub const NODE_BITS_SERVER: u32 = 24;
pub const NODE_BITS_WORKER: u32 = 22;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    Dht,
    Nostr,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Dht => "dht",
            Layer::Nostr => "nostr",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Metric {
    /// Distinct hashes a node gave us (after the seen filter).
    Hashes,
    /// DHT torrents stored whose hash was first heard from there.
    Stored,
    /// Nodes heard from, once per UTC day.
    Nodes,
    /// New events / torrent events relays there delivered.
    Events,
    Torrents,
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Hashes => "hashes",
            Metric::Stored => "stored",
            Metric::Nodes => "nodes",
            Metric::Events => "events",
            Metric::Torrents => "torrents",
        }
    }
}

/// Metrics of a layer, in the order of `CrawlMap.metrics`.
pub fn metrics_of(layer: Layer) -> &'static [&'static str] {
    match layer {
        Layer::Dht => &["hashes", "stored", "nodes"],
        Layer::Nostr => &["relays", "events", "torrents"],
    }
}

/// Distinct addresses per UTC day, without keeping them: a Bloom filter over a keyed SipHash
/// whose random key is replaced (and the bits cleared) when the day changes. Never persisted.
struct NodeFilter {
    bits: Vec<u64>,
    keys: RandomState,
    day: i64,
}

impl NodeFilter {
    fn new(log2_bits: u32) -> Self {
        Self {
            bits: vec![0; 1 << log2_bits.saturating_sub(6)],
            keys: RandomState::new(),
            day: i64::MIN,
        }
    }

    /// True the first time `ip` is offered today (and for ~3 % of repeats it says false for a
    /// new one: the count is a slight undercount).
    fn insert(&mut self, ip: Ipv4Addr, now: i64) -> bool {
        let day = now.div_euclid(86_400);
        if day != self.day {
            self.bits.fill(0);
            self.keys = RandomState::new();
            self.day = day;
        }
        let h = self.keys.hash_one(ip.octets());
        let m = (self.bits.len() * 64) as u64;
        let (a, b) = (h & 0xffff_ffff, (h >> 32) | 1);
        let mut new = false;
        for i in 0..3u64 {
            let bit = a.wrapping_add(i.wrapping_mul(b)) % m;
            let (w, mask) = ((bit / 64) as usize, 1u64 << (bit % 64));
            if self.bits[w] & mask == 0 {
                new = true;
                self.bits[w] |= mask;
            }
        }
        new
    }
}

/// Aggregate counts waiting to be written, and AS names to record.
#[derive(Default)]
pub struct Tally {
    counts: HashMap<(Layer, Metric, GeoCode), u64>,
    names: HashMap<u32, String>,
    named: HashSet<u32>,
}

impl Tally {
    fn add(&mut self, layer: Layer, metric: Metric, code: GeoCode, n: u64) {
        if n > 0 {
            *self.counts.entry((layer, metric, code)).or_default() += n;
        }
    }

    fn wants_name(&self, asn: u32) -> bool {
        asn != 0 && !self.named.contains(&asn)
    }

    fn name(&mut self, asn: u32, org: String) {
        if asn == 0 || org.is_empty() {
            return;
        }
        if self.named.len() >= NAMED_MAX {
            self.named.clear();
        }
        self.named.insert(asn);
        self.names.insert(asn, org);
    }
}

/// One machine's stored total over time (for `stored_per_min`).
type RateRing = VecDeque<(i64, u64)>;

/// Shared by the DHT pipeline, the worker endpoints, the API and [`server_loop`].
pub struct CrawlMap {
    mode: GeoMode,
    db: RwLock<Option<Arc<dyn Resolver>>>,
    unavailable: Mutex<Option<Unavailable>>,
    tally: Mutex<Tally>,
    nodes: Mutex<NodeFilter>,
    map_cache: Mutex<HashMap<(Layer, bool), (Instant, CrawlMapAnswer)>>,
    sources_cache: Mutex<Option<(Instant, CrawlSources)>>,
    routes_cache: tokio::sync::Mutex<Option<(Instant, Vec<DhtRouteCount>)>>,
    relay_snap: Mutex<Option<HashMap<String, (i64, i64)>>>,
    rates: Mutex<HashMap<String, RateRing>>,
}

impl Default for CrawlMap {
    fn default() -> Self {
        Self::new(GeoMode::On, NODE_BITS_SERVER)
    }
}

impl std::fmt::Debug for CrawlMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrawlMap")
            .field("mode", &self.mode)
            .field("available", &self.resolver().is_some())
            .finish()
    }
}

impl CrawlMap {
    pub fn new(mode: GeoMode, node_bits: u32) -> Self {
        Self {
            mode,
            db: RwLock::default(),
            unavailable: Mutex::new(Some(if mode == GeoMode::Off {
                Unavailable::Disabled
            } else {
                Unavailable::Missing
            })),
            tally: Mutex::default(),
            nodes: Mutex::new(NodeFilter::new(node_bits)),
            map_cache: Mutex::default(),
            sources_cache: Mutex::default(),
            routes_cache: tokio::sync::Mutex::default(),
            relay_snap: Mutex::default(),
            rates: Mutex::default(),
        }
    }

    pub fn mode(&self) -> GeoMode {
        self.mode
    }

    /// Install (or replace) the database; counting starts.
    pub fn set_resolver(&self, r: Arc<dyn Resolver>) {
        if self.mode == GeoMode::Off {
            return;
        }
        *self.db.write().unwrap() = Some(r);
        *self.unavailable.lock().unwrap() = None;
    }

    pub fn set_unavailable(&self, why: Unavailable) {
        *self.unavailable.lock().unwrap() = Some(why);
    }

    pub fn resolver(&self) -> Option<Arc<dyn Resolver>> {
        self.db.read().unwrap().clone()
    }

    /// Resolve and forget: the code, recording the AS name the first time an AS shows up.
    fn code_of(&self, db: &dyn Resolver, ip: IpAddr) -> GeoCode {
        let hit = db.lookup(ip, false);
        if self.tally.lock().unwrap().wants_name(hit.code.asn)
            && let Some(org) = db.lookup(ip, true).org
        {
            self.tally.lock().unwrap().name(hit.code.asn, org);
        }
        hit.code
    }

    /// A DHT node told us about a hash: counted once per node and UTC day (`nodes`).
    pub fn dht_node(&self, from: Option<SocketAddrV4>, now: i64) {
        let (Some(a), Some(db)) = (from, self.resolver()) else {
            return;
        };
        if !self.nodes.lock().unwrap().insert(*a.ip(), now) {
            return;
        }
        let code = self.code_of(db.as_ref(), IpAddr::V4(*a.ip()));
        self.tally
            .lock()
            .unwrap()
            .add(Layer::Dht, Metric::Nodes, code, 1);
    }

    /// Codes of the nodes that told us about these hashes (None: no database, or no address).
    /// `count`: also count them as `hashes` here (the server; a worker's are counted by the
    /// server from its triage request).
    pub fn dht_hashes(
        &self,
        from: impl IntoIterator<Item = Option<SocketAddrV4>>,
        count: bool,
    ) -> Vec<Option<GeoCode>> {
        let db = self.resolver();
        let out: Vec<Option<GeoCode>> = from
            .into_iter()
            .map(|f| match (&db, f) {
                (Some(db), Some(a)) => Some(self.code_of(db.as_ref(), IpAddr::V4(*a.ip()))),
                (Some(_), None) => Some(GeoCode::UNKNOWN),
                _ => None,
            })
            .collect();
        if count {
            let mut t = self.tally.lock().unwrap();
            for c in out.iter().flatten() {
                t.add(Layer::Dht, Metric::Hashes, *c, 1);
            }
        }
        out
    }

    pub fn add(&self, layer: Layer, metric: Metric, code: GeoCode, n: u64) {
        self.tally.lock().unwrap().add(layer, metric, code, n);
    }

    /// New DHT torrents and where their hash came from (`PersistOutcome.stored_geo`).
    pub fn add_stored(&self, codes: &[GeoCode]) {
        let mut t = self.tally.lock().unwrap();
        for c in codes {
            t.add(Layer::Dht, Metric::Stored, *c, 1);
        }
    }

    pub fn add_name(&self, asn: u32, org: &str) {
        let org = geo::clean_org(org);
        let mut t = self.tally.lock().unwrap();
        if t.wants_name(asn) {
            t.name(asn, org);
        }
    }

    /// Worker: the node counts and new AS names to send with the next triage request.
    pub fn take_upload(
        &self,
    ) -> (
        Vec<crate::fleet::proto::GeoTally>,
        Vec<crate::fleet::proto::AsName>,
    ) {
        let mut t = self.tally.lock().unwrap();
        let mut rows = Vec::new();
        t.counts.retain(|(l, m, c), n| {
            if *l == Layer::Dht && *m == Metric::Nodes && rows.len() < MAX_UPLOAD_ROWS {
                rows.push(crate::fleet::proto::GeoTally {
                    cc: c.country().map(str::to_string),
                    asn: c.asn(),
                    n: *n,
                });
                false
            } else {
                true
            }
        });
        let names: Vec<crate::fleet::proto::AsName> = t
            .names
            .drain()
            .take(MAX_UPLOAD_ROWS)
            .map(|(asn, org)| crate::fleet::proto::AsName { asn, org })
            .collect();
        (rows, names)
    }

    /// Server: what a worker sent with its triage request.
    pub fn add_upload(
        &self,
        nodes: &[crate::fleet::proto::GeoTally],
        names: &[crate::fleet::proto::AsName],
    ) {
        if self.mode == GeoMode::Off {
            return;
        }
        let mut t = self.tally.lock().unwrap();
        for r in nodes.iter().take(MAX_UPLOAD_ROWS) {
            let code = GeoCode::from_parts(r.cc.as_deref(), r.asn);
            t.add(Layer::Dht, Metric::Nodes, code, r.n.min(1 << 32));
        }
        for n in names.iter().take(MAX_UPLOAD_ROWS) {
            let org = geo::clean_org(&n.org);
            if t.wants_name(n.asn) {
                t.name(n.asn, org);
            }
        }
    }

    /// Write the tally (hourly bucket of `now` and the totals) and pending AS names. On error
    /// the counts go back into the tally.
    pub async fn flush(&self, pool: &PgPool, now: i64) -> sqlx::Result<usize> {
        let (counts, names) = {
            let mut t = self.tally.lock().unwrap();
            (std::mem::take(&mut t.counts), std::mem::take(&mut t.names))
        };
        if counts.is_empty() && names.is_empty() {
            return Ok(0);
        }
        let rows: BTreeMap<_, _> = counts.iter().map(|(k, v)| (*k, *v)).collect();
        let r = write_counts(pool, &rows, &names, now).await;
        if r.is_err() {
            let mut t = self.tally.lock().unwrap();
            for ((l, m, c), n) in counts {
                t.add(l, m, c, n);
            }
            for (a, o) in names {
                t.names.entry(a).or_insert(o);
            }
        }
        r.map(|_| rows.len())
    }

    /// Resolve the hosts of relays not checked for a day (a batch); returns how many.
    pub async fn resolve_relays(&self, pool: &PgPool, now: i64) -> sqlx::Result<usize> {
        let Some(db) = self.resolver() else {
            return Ok(0);
        };
        let urls: Vec<String> = sqlx::query_scalar(
            "SELECT url FROM relays
             WHERE geo_checked_at IS NULL OR geo_checked_at < $1
             ORDER BY events_found DESC, url LIMIT $2",
        )
        .bind(now - RELAY_RECHECK_SECS)
        .bind(RELAY_BATCH)
        .fetch_all(pool)
        .await?;
        if urls.is_empty() {
            return Ok(0);
        }
        let codes: Vec<(String, GeoCode)> = futures::stream::iter(urls)
            .map(|u| {
                let db = db.clone();
                async move {
                    let code = match relay_addr(&u).await {
                        // The address is used for this lookup only.
                        Some(ip) => self.code_of(db.as_ref(), ip),
                        None => GeoCode::UNKNOWN,
                    };
                    (u, code)
                }
            })
            .buffer_unordered(16)
            .collect()
            .await;
        let n = codes.len();
        let urls: Vec<&str> = codes.iter().map(|(u, _)| u.as_str()).collect();
        let ccs: Vec<String> = codes.iter().map(|(_, c)| c.cc_text()).collect();
        let asns: Vec<i32> = codes
            .iter()
            .map(|(_, c)| i32::try_from(c.asn).unwrap_or(0))
            .collect();
        sqlx::query(
            "UPDATE relays r SET geo_cc = u.cc, geo_asn = u.asn, geo_checked_at = $4
             FROM unnest($1::text[], $2::text[], $3::int4[]) AS u(url, cc, asn)
             WHERE r.url = u.url",
        )
        .bind(&urls)
        .bind(&ccs)
        .bind(&asns)
        .bind(now)
        .execute(pool)
        .await?;
        Ok(n)
    }

    /// Relays' event counters since the previous snapshot, by where they resolved (the
    /// `nostr` layer's hourly rows). The first snapshot after a start is the baseline.
    pub async fn snapshot_relays(&self, pool: &PgPool) -> sqlx::Result<()> {
        if self.mode == GeoMode::Off {
            return Ok(());
        }
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, i64, i64, Option<String>, Option<i32>)> =
            sqlx::query_as("SELECT url, events_found, torrents_found, geo_cc, geo_asn FROM relays")
                .fetch_all(pool)
                .await?;
        let mut snap = self.relay_snap.lock().unwrap();
        let first = snap.is_none();
        let prev = snap.get_or_insert_with(HashMap::new);
        let mut t = self.tally.lock().unwrap();
        for (url, ev, tor, cc, asn) in rows {
            if !first {
                let (pe, pt) = prev.get(&url).copied().unwrap_or((0, 0));
                let code =
                    GeoCode::from_parts(cc.as_deref(), asn.and_then(|a| u32::try_from(a).ok()));
                t.add(Layer::Nostr, Metric::Events, code, (ev - pe).max(0) as u64);
                t.add(
                    Layer::Nostr,
                    Metric::Torrents,
                    code,
                    (tor - pt).max(0) as u64,
                );
            }
            prev.insert(url, (ev, tor));
        }
        Ok(())
    }

    /// Record each machine's stored total (for `stored_per_min`, ~10 minutes).
    pub fn sample_rates(&self, machines: &[MachineSources], now: i64) {
        let mut r = self.rates.lock().unwrap();
        for m in machines {
            let ring = r.entry(m.name.clone()).or_default();
            ring.push_back((now, m.dht_stored));
            while ring.len() > 11 || ring.front().is_some_and(|(t, _)| now - t > 660) {
                ring.pop_front();
            }
        }
    }

    fn rate(&self, name: &str, now: i64, stored: u64) -> f64 {
        let r = self.rates.lock().unwrap();
        match r.get(name).and_then(|ring| ring.front()) {
            Some((t, s)) if now - t >= 60 && stored >= *s => {
                (stored - s) as f64 / ((now - t) as f64 / 60.0)
            }
            _ => 0.0,
        }
    }

    /// `GET /api/v1/stats/map`.
    pub async fn map(
        &self,
        pool: &PgPool,
        layer: Layer,
        day: bool,
        now: i64,
    ) -> sqlx::Result<CrawlMapAnswer> {
        if let Some((at, a)) = self.map_cache.lock().unwrap().get(&(layer, day))
            && at.elapsed() < MAP_TTL
        {
            return Ok(a.clone());
        }
        let a = self.compute_map(pool, layer, day, now).await?;
        self.map_cache
            .lock()
            .unwrap()
            .insert((layer, day), (Instant::now(), a.clone()));
        Ok(a)
    }

    async fn compute_map(
        &self,
        pool: &PgPool,
        layer: Layer,
        day: bool,
        now: i64,
    ) -> sqlx::Result<CrawlMapAnswer> {
        let since_meta: Option<i64> =
            sqlx::query_scalar("SELECT value FROM meta WHERE key = 'crawl_map_since'")
                .fetch_optional(pool)
                .await?;
        let from = now - 86_400;
        let metrics = metrics_of(layer);
        let idx = |m: &str| metrics.iter().position(|x| *x == m);
        // (metric index, code, value)
        let mut rows: Vec<(usize, GeoCode, u64)> = Vec::new();
        let counters: Vec<(String, String, i32, i64)> = if day {
            sqlx::query_as(
                "SELECT metric, cc, asn, sum(value)::int8 FROM geo_counts_hourly
                 WHERE layer = $1 AND bucket >= $2 GROUP BY 1, 2, 3",
            )
            .bind(layer.as_str())
            .bind(from - from.rem_euclid(3600) + 3600)
            .fetch_all(pool)
            .await?
        } else if layer == Layer::Dht {
            sqlx::query_as("SELECT metric, cc, asn, value FROM geo_counts_total WHERE layer = $1")
                .bind(layer.as_str())
                .fetch_all(pool)
                .await?
        } else {
            Vec::new()
        };
        for (m, cc, asn, v) in counters {
            if let Some(i) = idx(&m) {
                rows.push((
                    i,
                    GeoCode::from_parts(Some(&cc), u32::try_from(asn).ok()),
                    v.max(0) as u64,
                ));
            }
        }
        if layer == Layer::Nostr {
            // Relays by where they resolved; "all": their lifetime event counters too.
            let relays: Vec<(String, i32, i64, i64, i64)> = sqlx::query_as(
                "SELECT geo_cc, COALESCE(geo_asn, 0), count(*),
                        COALESCE(sum(events_found), 0)::int8, COALESCE(sum(torrents_found), 0)::int8
                 FROM relays
                 WHERE geo_cc IS NOT NULL AND ($1::int8 IS NULL OR last_ok >= $1)
                 GROUP BY 1, 2",
            )
            .bind(day.then_some(from))
            .fetch_all(pool)
            .await?;
            for (cc, asn, n, ev, tor) in relays {
                let code = GeoCode::from_parts(Some(&cc), u32::try_from(asn).ok());
                rows.push((0, code, n.max(0) as u64));
                if !day {
                    rows.push((1, code, ev.max(0) as u64));
                    rows.push((2, code, tor.max(0) as u64));
                }
            }
        }
        let mut a = aggregate(metrics.len(), &rows);
        // AS names of the networks shown.
        let asns: Vec<i32> = a
            .networks
            .iter()
            .filter_map(|n| n.asn.and_then(|x| i32::try_from(x).ok()))
            .collect();
        if !asns.is_empty() {
            let names: HashMap<i32, String> = sqlx::query_as::<_, (i32, String)>(
                "SELECT asn, org FROM geo_as_names WHERE asn = ANY($1)",
            )
            .bind(&asns)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
            for n in &mut a.networks {
                n.as_org = n
                    .asn
                    .and_then(|x| i32::try_from(x).ok())
                    .and_then(|x| names.get(&x).cloned());
            }
        }
        let db = self.resolver();
        a.layer = layer.as_str().into();
        a.window = if day { "day" } else { "all" }.into();
        a.available = db.is_some();
        a.reason = self
            .unavailable
            .lock()
            .unwrap()
            .filter(|_| db.is_none())
            .map(|u| u.as_str().to_string());
        a.database = db.map(|d| d.info());
        a.attribution = geo::ATTRIBUTION.into();
        a.metrics = metrics.iter().map(|s| s.to_string()).collect();
        a.since = since_meta.map(|s| if day { s.max(from) } else { s });
        a.computed_at = now;
        Ok(a)
    }

    /// `GET /api/v1/stats/sources`.
    pub async fn sources(
        &self,
        pool: &PgPool,
        machines: Vec<MachineSources>,
        now: i64,
    ) -> sqlx::Result<CrawlSources> {
        let machines: Vec<MachineSources> = machines
            .into_iter()
            .map(|mut m| {
                m.stored_per_min = self.rate(&m.name, now, m.dht_stored);
                m
            })
            .collect();
        if let Some((at, s)) = &*self.sources_cache.lock().unwrap()
            && at.elapsed() < SOURCES_TTL
        {
            let mut s = s.clone();
            s.machines = machines;
            return Ok(s);
        }
        let since: Option<i64> =
            sqlx::query_scalar("SELECT value FROM meta WHERE key = 'crawl_map_since'")
                .fetch_optional(pool)
                .await?;
        let routes = self.routes(pool).await?;
        let hourly = route_hours(pool, now).await?;
        let by_source: Vec<(String, i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT source, count(*), count(*) FILTER (WHERE enabled),
                    count(*) FILTER (WHERE events_found > 0),
                    COALESCE(sum(events_found), 0)::int8, COALESCE(sum(torrents_found), 0)::int8
             FROM relays GROUP BY source ORDER BY 5 DESC, 1",
        )
        .fetch_all(pool)
        .await?;
        #[allow(clippy::type_complexity)]
        let top: Vec<(
            String,
            String,
            bool,
            bool,
            i64,
            i64,
            Option<String>,
            Option<i32>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT r.url, r.source, r.enabled, COALESCE(r.last_ok >= $1, false),
                        r.events_found, r.torrents_found, r.geo_cc, r.geo_asn, n.org
                 FROM relays r LEFT JOIN geo_as_names n ON n.asn = r.geo_asn
                 WHERE r.events_found > 0
                 ORDER BY r.events_found DESC, r.torrents_found DESC, r.url LIMIT $2",
        )
        .bind(now - 86_400)
        .bind(TOP_RELAYS)
        .fetch_all(pool)
        .await?;
        let u = |x: i64| x.max(0) as u64;
        let s = CrawlSources {
            dht: DhtSources {
                routes,
                hourly,
                hourly_since: since,
            },
            nostr: NostrSources {
                by_source: by_source
                    .into_iter()
                    .map(|(source, relays, enabled, delivered, events, torrents)| {
                        RelaySourceCount {
                            source,
                            relays: u(relays),
                            enabled: u(enabled),
                            delivered: u(delivered),
                            events: u(events),
                            torrents: u(torrents),
                        }
                    })
                    .collect(),
                top_relays: top
                    .into_iter()
                    .map(|(url, source, enabled, ok, ev, tor, cc, asn, org)| {
                        let code = cc.as_deref().map(|c| {
                            GeoCode::from_parts(Some(c), asn.and_then(|a| u32::try_from(a).ok()))
                        });
                        RelayYield {
                            url: redact_ip_host(&url),
                            source,
                            enabled,
                            ok_24h: ok,
                            events: u(ev),
                            torrents: u(tor),
                            country: code.and_then(|c| c.country().map(str::to_string)),
                            asn: code.and_then(|c| c.asn()),
                            as_org: org,
                        }
                    })
                    .collect(),
                torrents_since: since,
            },
            machines: Vec::new(),
            computed_at: now,
        };
        *self.sources_cache.lock().unwrap() = Some((Instant::now(), s.clone()));
        let mut s = s;
        s.machines = machines;
        Ok(s)
    }

    /// Distinct hashes per route: rows still in `dht_seen` plus stored ones (which left it).
    async fn routes(&self, pool: &PgPool) -> sqlx::Result<Vec<DhtRouteCount>> {
        let mut cache = self.routes_cache.lock().await;
        if let Some((at, r)) = &*cache
            && at.elapsed() < ROUTES_TTL
        {
            return Ok(r.clone());
        }
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(ROUTES_TIMEOUT_MS.to_string())
            .execute(&mut *tx)
            .await?;
        let seen: Vec<(i16, i64)> =
            sqlx::query_as("SELECT via, count(*) FROM dht_seen GROUP BY via")
                .fetch_all(&mut *tx)
                .await?;
        let stored: Vec<(String, i64)> =
            sqlx::query_as("SELECT discovered_via, count(*) FROM dht_torrents GROUP BY 1")
                .fetch_all(&mut *tx)
                .await?;
        tx.commit().await?;
        let names = ["sample", "get_peers", "announce"];
        let code = |v: &str| match v {
            "sample" => 0i16,
            "announce" => 1,
            _ => 2,
        };
        let out: Vec<DhtRouteCount> = names
            .iter()
            .map(|n| {
                let st = stored
                    .iter()
                    .filter(|(v, _)| v == n)
                    .map(|(_, c)| *c)
                    .sum::<i64>()
                    .max(0) as u64;
                let sn = seen
                    .iter()
                    .filter(|(v, _)| *v == code(n))
                    .map(|(_, c)| *c)
                    .sum::<i64>()
                    .max(0) as u64;
                DhtRouteCount {
                    via: n.to_string(),
                    seen: sn + st,
                    stored: st,
                }
            })
            .collect();
        *cache = Some((Instant::now(), out.clone()));
        Ok(out)
    }
}

/// `stats_series` per-route metrics (migration `20261103000000`).
pub const ROUTE_SERIES: [&str; 6] = [
    "dht_seen_sample",
    "dht_seen_get_peers",
    "dht_seen_announce",
    "dht_stored_sample",
    "dht_stored_get_peers",
    "dht_stored_announce",
];

async fn route_hours(pool: &PgPool, now: i64) -> sqlx::Result<Vec<DhtRouteHour>> {
    let last = now - now.rem_euclid(3600);
    let first = last - 47 * 3600;
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT metric, bucket, value FROM stats_series WHERE metric = ANY($1) AND bucket >= $2",
    )
    .bind(ROUTE_SERIES.as_slice())
    .bind(first)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<DhtRouteHour> = (0..48)
        .map(|i| DhtRouteHour {
            bucket: first + i * 3600,
            ..Default::default()
        })
        .collect();
    for (m, b, v) in rows {
        let Some(h) = out.get_mut(((b - first) / 3600) as usize) else {
            continue;
        };
        let v = v.max(0) as u64;
        let (t, slot) = match m
            .split_once('_')
            .map(|x| x.1)
            .and_then(|r| r.split_once('_'))
        {
            Some(("seen", r)) => (&mut h.seen, r),
            Some(("stored", r)) => (&mut h.stored, r),
            _ => continue,
        };
        match slot {
            "sample" => t.sample += v,
            "get_peers" => t.get_peers += v,
            "announce" => t.announce += v,
            _ => {}
        }
    }
    Ok(out)
}

async fn write_counts(
    pool: &PgPool,
    rows: &BTreeMap<(Layer, Metric, GeoCode), u64>,
    names: &HashMap<u32, String>,
    now: i64,
) -> sqlx::Result<()> {
    let bucket = now - now.rem_euclid(3600);
    let layers: Vec<&str> = rows.keys().map(|(l, _, _)| l.as_str()).collect();
    let metrics: Vec<&str> = rows.keys().map(|(_, m, _)| m.as_str()).collect();
    let ccs: Vec<String> = rows.keys().map(|(_, _, c)| c.cc_text()).collect();
    let asns: Vec<i32> = rows
        .keys()
        .map(|(_, _, c)| i32::try_from(c.asn).unwrap_or(0))
        .collect();
    let values: Vec<i64> = rows
        .values()
        .map(|v| i64::try_from(*v).unwrap_or(i64::MAX))
        .collect();
    let mut tx = pool.begin().await?;
    if !rows.is_empty() {
        sqlx::query(
            "INSERT INTO geo_counts_hourly AS g (layer, metric, bucket, cc, asn, value)
             SELECT l, m, $6, c, a, v FROM unnest($1::text[], $2::text[], $3::text[], $4::int4[], $5::int8[])
                 AS u(l, m, c, a, v)
             ON CONFLICT (layer, metric, bucket, cc, asn) DO UPDATE SET value = g.value + EXCLUDED.value",
        )
        .bind(&layers)
        .bind(&metrics)
        .bind(&ccs)
        .bind(&asns)
        .bind(&values)
        .bind(bucket)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO geo_counts_total AS g (layer, metric, cc, asn, value)
             SELECT l, m, c, a, v FROM unnest($1::text[], $2::text[], $3::text[], $4::int4[], $5::int8[])
                 AS u(l, m, c, a, v)
             ON CONFLICT (layer, metric, cc, asn) DO UPDATE SET value = g.value + EXCLUDED.value",
        )
        .bind(&layers)
        .bind(&metrics)
        .bind(&ccs)
        .bind(&asns)
        .bind(&values)
        .execute(&mut *tx)
        .await?;
    }
    if !names.is_empty() {
        let mut v: Vec<(&u32, &String)> = names.iter().collect();
        v.sort();
        let a: Vec<i32> = v
            .iter()
            .map(|(a, _)| i32::try_from(**a).unwrap_or(0))
            .collect();
        let o: Vec<&str> = v.iter().map(|(_, o)| o.as_str()).collect();
        sqlx::query(
            "INSERT INTO geo_as_names (asn, org, updated_at)
             SELECT a, o, $3 FROM unnest($1::int4[], $2::text[]) AS u(a, o) WHERE a > 0
             ON CONFLICT (asn) DO UPDATE SET org = EXCLUDED.org, updated_at = EXCLUDED.updated_at",
        )
        .bind(&a)
        .bind(&o)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Countries, top networks, unknown and totals from (metric index, code, value) rows.
pub fn aggregate(n: usize, rows: &[(usize, GeoCode, u64)]) -> CrawlMapAnswer {
    let mut countries: HashMap<[u8; 2], Vec<u64>> = HashMap::new();
    // AS number → (counts, first-metric count per country).
    type Net = (Vec<u64>, HashMap<[u8; 2], u64>);
    let mut nets: HashMap<u32, Net> = HashMap::new();
    let mut unknown = vec![0u64; n];
    let mut totals = vec![0u64; n];
    for (i, c, v) in rows {
        let (i, v) = (*i, *v);
        if i >= n {
            continue;
        }
        totals[i] += v;
        match c.country() {
            Some(_) => countries.entry(c.cc).or_insert_with(|| vec![0; n])[i] += v,
            None => unknown[i] += v,
        }
        if c.asn != 0 {
            let e = nets
                .entry(c.asn)
                .or_insert_with(|| (vec![0; n], HashMap::new()));
            e.0[i] += v;
            if c.country().is_some() {
                *e.1.entry(c.cc).or_default() += v;
            }
        }
    }
    let mut cs: Vec<GeoCount> = countries
        .into_iter()
        .map(|(cc, counts)| GeoCount {
            country: Some(String::from_utf8_lossy(&cc).into_owned()),
            counts,
            ..Default::default()
        })
        .collect();
    cs.sort_by(|a, b| {
        b.counts
            .cmp(&a.counts)
            .then_with(|| a.country.cmp(&b.country))
    });
    let mut ns: Vec<GeoCount> = nets
        .into_iter()
        .map(|(asn, (counts, by_cc))| GeoCount {
            country: by_cc
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(cc, _)| String::from_utf8_lossy(&cc).into_owned()),
            asn: Some(asn),
            as_org: None,
            counts,
        })
        .collect();
    ns.sort_by(|a, b| b.counts.cmp(&a.counts).then_with(|| a.asn.cmp(&b.asn)));
    ns.truncate(TOP_NETWORKS);
    CrawlMapAnswer {
        countries: cs,
        networks: ns,
        unknown,
        totals,
        ..Default::default()
    }
}

/// The address a relay URL's host resolves to (IPv4 first), for one lookup. Onion / I2P /
/// local names and failures: None.
async fn relay_addr(url: &str) -> Option<IpAddr> {
    let u = url::Url::parse(url).ok()?;
    let port = u.port_or_known_default().unwrap_or(443);
    match u.host()? {
        url::Host::Ipv4(a) => Some(IpAddr::V4(a)),
        url::Host::Ipv6(a) => Some(IpAddr::V6(a)),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if [".onion", ".i2p", ".local", ".localhost", ".internal"]
                .iter()
                .any(|s| d.ends_with(s))
                || d == "localhost"
            {
                return None;
            }
            let addrs: Vec<IpAddr> =
                tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((d.as_str(), port)))
                    .await
                    .ok()?
                    .ok()?
                    .map(|s| s.ip())
                    .collect();
            addrs
                .iter()
                .find(|a| a.is_ipv4())
                .or(addrs.first())
                .copied()
        }
    }
}

/// A relay URL whose host is an IP literal is shown without it (D101 applies to relays too).
pub fn redact_ip_host(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) if matches!(u.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_))) => {
            format!("{}://(IP address)", u.scheme())
        }
        _ => url.to_string(),
    }
}

/// Keep the database current (download / monthly refresh) and installed in `map`.
pub async fn refresh_loop(map: Arc<CrawlMap>, cfg: GeoConfig, mut stop: watch::Receiver<bool>) {
    if cfg.mode == GeoMode::Off {
        map.set_unavailable(Unavailable::Disabled);
        return;
    }
    loop {
        let c = cfg.clone();
        let r = geo::refresh(&c, magnetar_core::now()).await;
        match r {
            Ok(db) => {
                tracing::info!(
                    "geo: {} (country build {}, ASN build {:?}) from {}",
                    if db.asn_build().is_some() {
                        "country + ASN"
                    } else {
                        "country only"
                    },
                    db.country_build(),
                    db.asn_build(),
                    cfg.dir.display()
                );
                map.set_resolver(Arc::new(db));
            }
            Err((why, e)) => {
                tracing::warn!("geo: crawl map unavailable ({}): {e}", why.as_str());
                if map.resolver().is_none() {
                    map.set_unavailable(why);
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(REFRESH_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// Server: flush the tally every minute, prune old hours, resolve relays, snapshot relay
/// counters, and sample machine rates via `machines`.
pub async fn server_loop(
    map: Arc<CrawlMap>,
    pool: PgPool,
    machines: impl Fn() -> Vec<MachineSources> + Send + 'static,
    mut stop: watch::Receiver<bool>,
) {
    let mut last_relays: Option<Instant> = None;
    let mut last_snap: Option<Instant> = None;
    let mut last_prune: Option<Instant> = None;
    let mut errors = 0u64;
    loop {
        let stopping = tokio::select! {
            _ = tokio::time::sleep(FLUSH_EVERY) => false,
            _ = stop_requested(&mut stop) => true,
        };
        let now = magnetar_core::now();
        map.sample_rates(&machines(), now);
        if !stopping && last_snap.is_none_or(|t| t.elapsed() >= RELAY_SNAPSHOT_EVERY) {
            last_snap = Some(Instant::now());
            if let Err(e) = map.snapshot_relays(&pool).await {
                tracing::warn!(
                    "crawl map: relay counters: {}",
                    crate::dht::persist::error_class(&e)
                );
            }
        }
        if let Err(e) = map.flush(&pool, now).await {
            if errors.is_multiple_of(10) {
                tracing::warn!(
                    "crawl map: writing counters: {}",
                    crate::dht::persist::error_class(&e)
                );
            }
            errors += 1;
        }
        if stopping {
            return;
        }
        if last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600)) {
            last_prune = Some(Instant::now());
            let _ = sqlx::query("DELETE FROM geo_counts_hourly WHERE bucket < $1")
                .bind(now - HOURLY_KEEP_SECS)
                .execute(&pool)
                .await;
        }
        if map.resolver().is_some() && last_relays.is_none_or(|t| t.elapsed() >= RELAY_EVERY) {
            last_relays = Some(Instant::now());
            match map.resolve_relays(&pool, now).await {
                Ok(n) if n > 0 => tracing::info!("crawl map: located {n} relay host(s)"),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    "crawl map: relays: {}",
                    crate::dht::persist::error_class(&e)
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_filter_counts_once_per_day() {
        let mut f = NodeFilter::new(16);
        let a: Ipv4Addr = "198.51.100.7".parse().unwrap();
        assert!(f.insert(a, 86_400 * 10 + 5));
        assert!(!f.insert(a, 86_400 * 10 + 500));
        // A new UTC day: new key, cleared bits.
        assert!(f.insert(a, 86_400 * 11 + 1));
        let mut new = 0;
        for i in 0..1000u32 {
            if f.insert(Ipv4Addr::from(0x0a00_0000 + i), 86_400 * 11 + 2) {
                new += 1;
            }
        }
        assert!(new > 990, "{new}");
    }

    #[test]
    fn aggregates_countries_and_networks() {
        let fr = GeoCode::from_parts(Some("FR"), Some(3215));
        let fr2 = GeoCode::from_parts(Some("FR"), Some(12322));
        let de = GeoCode::from_parts(Some("DE"), Some(3215));
        let unk = GeoCode::from_parts(None, Some(64512));
        let a = aggregate(
            2,
            &[
                (0, fr, 10),
                (1, fr, 2),
                (0, fr2, 5),
                (0, de, 1),
                (0, unk, 4),
                (5, fr, 99),
            ],
        );
        assert_eq!(a.totals, vec![20, 2]);
        assert_eq!(a.unknown, vec![4, 0]);
        assert_eq!(a.countries[0].country.as_deref(), Some("FR"));
        assert_eq!(a.countries[0].counts, vec![15, 2]);
        assert_eq!(a.networks[0].asn, Some(3215));
        assert_eq!(a.networks[0].counts, vec![11, 2]);
        assert_eq!(a.networks[0].country.as_deref(), Some("FR"));
        assert!(
            a.networks
                .iter()
                .any(|n| n.asn == Some(64512) && n.country.is_none())
        );
    }

    #[test]
    fn ip_literal_relays_are_redacted() {
        assert_eq!(
            redact_ip_host("wss://203.0.113.5:7447/"),
            "wss://(IP address)"
        );
        assert_eq!(redact_ip_host("wss://[2001:db8::1]/"), "wss://(IP address)");
        assert_eq!(
            redact_ip_host("wss://relay.example.com/"),
            "wss://relay.example.com/"
        );
    }

    #[test]
    fn tally_upload_round_trip() {
        let m = CrawlMap::new(GeoMode::On, 16);
        m.set_resolver(Arc::new(geo::TableResolver {
            rows: vec![(
                "203.0.113.0".parse().unwrap(),
                24,
                GeoCode::from_parts(Some("NZ"), Some(64500)),
                "Example AS",
            )],
        }));
        let a = Some("203.0.113.9:6881".parse().unwrap());
        m.dht_node(a, 1000);
        m.dht_node(a, 1001);
        let codes = m.dht_hashes([a, None], false);
        assert_eq!(codes[0].unwrap().country(), Some("NZ"));
        assert_eq!(codes[1], Some(GeoCode::UNKNOWN));
        let (rows, names) = m.take_upload();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].n, 1);
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].org, "Example AS");
        let server = CrawlMap::new(GeoMode::On, 16);
        server.add_upload(&rows, &names);
        let t = server.tally.lock().unwrap();
        assert_eq!(t.counts.values().sum::<u64>(), 1);
        assert_eq!(t.names.get(&64500).map(String::as_str), Some("Example AS"));
    }
}
