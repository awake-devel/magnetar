//! Network graphs for the app (ARCHITECTURE.md D47, docs/STATS.md): nodes + weighted edges,
//! at most `MAX_GRAPH_NODES`, laid out on the phone. Web of trust (root → follows →
//! publishers), relays ↔ publishers (NIP-65 write relays), one title's releases, one torrent's
//! swarm sources.

use std::collections::{HashMap, HashSet};

use magnetar_core::api::{Graph, GraphEdge, GraphNode, MAX_GRAPH_NODES};
use magnetar_core::error::Result;
use magnetar_core::magnet::DEFAULT_TRACKERS;
use magnetar_core::model::{LivenessSource, TrustFactor, npub};
use magnetar_core::query::SearchQuery;
use magnetar_core::release;
use magnetar_core::store::{Store, keys};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};

use crate::liveness::LivenessService;
use crate::store::{PgStore, db};

/// Publishers in the trust graph (with a root the rest of the cap goes to the follows that
/// connect them) and in the relay map.
const WOT_PUBLISHERS: usize = 100;
const WOT_PUBLISHERS_NO_ROOT: usize = 150;
const RELAY_PUBLISHERS: usize = 100;
/// Releases of one title (the most seeded).
const MEDIA_RELEASES: usize = 120;
/// Lines of trust explanation per node.
const MAX_DETAIL: usize = 6;

/// Nodes keyed by namespace + id, edges deduplicated.
#[derive(Default)]
struct Builder {
    g: Graph,
    index: HashMap<String, u32>,
    edges: HashSet<(u32, u32)>,
}

impl Builder {
    fn new(kind: &str) -> Self {
        Self {
            g: Graph {
                kind: kind.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn get(&self, ns: &str, id: &str) -> Option<u32> {
        self.index.get(&format!("{ns}:{id}")).copied()
    }

    /// The node `ns:id`, created with `make` if new. None once the cap is reached.
    fn node(&mut self, ns: &str, id: &str, make: impl FnOnce() -> GraphNode) -> Option<u32> {
        let key = format!("{ns}:{id}");
        if let Some(i) = self.index.get(&key) {
            return Some(*i);
        }
        if self.g.nodes.len() >= MAX_GRAPH_NODES {
            self.g.truncated = true;
            return None;
        }
        let i = self.g.nodes.len() as u32;
        self.g.nodes.push(make());
        self.index.insert(key, i);
        Some(i)
    }

    fn edge(&mut self, a: u32, b: u32, weight: f64, kind: &str) {
        if a != b && self.edges.insert((a, b)) {
            self.g.edges.push(GraphEdge {
                source: a,
                target: b,
                weight,
                kind: kind.into(),
            });
        }
    }

    fn done(self) -> Graph {
        self.g
    }
}

fn node(id: &str, kind: &str, label: String, weight: f64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        kind: kind.into(),
        label,
        weight,
        ..Default::default()
    }
}

/// Profile name, else the start of the npub.
fn pubkey_label(pk: &str, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.chars().take(40).collect(),
        None => {
            let n = npub(pk);
            format!("{}…", n.chars().take(14).collect::<String>())
        }
    }
}

/// "+25 followed by 3 of your follows", strongest first.
fn factor_lines(factors: &[TrustFactor]) -> Vec<String> {
    let mut f: Vec<&TrustFactor> = factors.iter().collect();
    f.sort_by_key(|f| -f.delta.abs());
    f.into_iter()
        .take(MAX_DETAIL)
        .map(|f| format!("{:+} {}", f.delta, f.detail))
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[derive(FromRow)]
struct PublisherRow {
    pubkey: String,
    torrents: i32,
    score: Option<i16>,
    factors: Option<Json<Vec<TrustFactor>>>,
    name: Option<String>,
}

async fn top_publishers(pool: &PgPool, limit: usize) -> Result<Vec<PublisherRow>> {
    sqlx::query_as(
        "SELECT pc.pubkey, pc.torrents, tc.score, tc.factors, p.name
         FROM publisher_counts pc
         LEFT JOIN trust_cache tc ON tc.pubkey = pc.pubkey
         LEFT JOIN profiles p ON p.pubkey = pc.pubkey
         ORDER BY pc.torrents DESC, pc.pubkey LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .map_err(db)
}

fn publisher_node(p: &PublisherRow, kind: &str) -> GraphNode {
    let mut detail = vec![plural(p.torrents.max(0) as usize, "torrent", "torrents")];
    if let Some(f) = &p.factors {
        detail.extend(factor_lines(&f.0));
    }
    GraphNode {
        score: p.score.map(|s| s.clamp(0, 100) as u8),
        detail,
        ..node(
            &p.pubkey,
            kind,
            pubkey_label(&p.pubkey, p.name.as_deref()),
            f64::from(p.torrents.max(0)),
        )
    }
}

async fn trust_root(store: &PgStore) -> Result<Option<String>> {
    Ok(store
        .get_setting(keys::TRUST_ROOT)
        .await?
        .and_then(|v| serde_json::from_str::<Option<String>>(&v).ok().flatten())
        .filter(|r| r.len() == 64))
}

#[derive(FromRow)]
struct PeopleRow {
    pubkey: String,
    name: Option<String>,
    score: Option<i16>,
}

async fn people(pool: &PgPool, pubkeys: &[String]) -> Result<HashMap<String, PeopleRow>> {
    let rows: Vec<PeopleRow> = sqlx::query_as(
        "SELECT k.pubkey, p.name, tc.score FROM unnest($1::text[]) AS k(pubkey)
         LEFT JOIN profiles p ON p.pubkey = k.pubkey
         LEFT JOIN trust_cache tc ON tc.pubkey = k.pubkey",
    )
    .bind(pubkeys)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    Ok(rows.into_iter().map(|r| (r.pubkey.clone(), r)).collect())
}

async fn follow_edges(
    pool: &PgPool,
    followers: &[String],
    followees: &[String],
) -> Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT follower, followee FROM follows
         WHERE follower = ANY($1) AND followee = ANY($2) ORDER BY follower, followee",
    )
    .bind(followers)
    .bind(followees)
    .fetch_all(pool)
    .await
    .map_err(db)
}

/// Web of trust: the trust root, its follows that follow the top publishers (most such
/// follows first), the publishers with their score and its explanation (D18 factors).
/// Without a root: the top publishers and who of them follows whom.
pub async fn wot(store: &PgStore) -> Result<Graph> {
    let pool = store.pool();
    let mut b = Builder::new("wot");
    let root = trust_root(store).await?;
    let pubs = top_publishers(
        pool,
        if root.is_some() {
            WOT_PUBLISHERS
        } else {
            WOT_PUBLISHERS_NO_ROOT
        },
    )
    .await?;
    let pub_keys: Vec<String> = pubs.iter().map(|p| p.pubkey.clone()).collect();
    let Some(root) = root else {
        for p in &pubs {
            b.node("pk", &p.pubkey, || publisher_node(p, "publisher"));
        }
        for (a, c) in follow_edges(pool, &pub_keys, &pub_keys).await? {
            if let (Some(x), Some(y)) = (b.get("pk", &a), b.get("pk", &c)) {
                b.edge(x, y, 1.0, "follows");
            }
        }
        let mut g = b.done();
        g.note = Some("No trust root is set: publishers and who of them follows whom.".into());
        return Ok(g);
    };

    let d1: Vec<String> = store.follows_of(&root).await?;
    let d1_set: HashSet<&str> = d1.iter().map(String::as_str).collect();
    let pub_set: HashSet<&str> = pub_keys.iter().map(String::as_str).collect();
    // Who among the root's follows (and the publishers themselves) follows which publisher.
    let mut askers: Vec<String> = d1
        .iter()
        .filter(|f| !pub_set.contains(f.as_str()))
        .cloned()
        .collect();
    askers.extend(pub_keys.iter().cloned());
    let edges = follow_edges(pool, &askers, &pub_keys).await?;
    let mut reach: HashMap<&str, usize> = HashMap::new();
    for (f, _) in &edges {
        if d1_set.contains(f.as_str()) && !pub_set.contains(f.as_str()) {
            *reach.entry(f.as_str()).or_default() += 1;
        }
    }
    let mut via: Vec<(&str, usize)> = reach.into_iter().collect();
    via.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let room = MAX_GRAPH_NODES.saturating_sub(1 + pubs.len());
    if via.len() > room {
        b.g.truncated = true;
        via.truncate(room);
    }
    let via_keys: Vec<String> = via.iter().map(|(k, _)| k.to_string()).collect();
    let mut ask: Vec<String> = via_keys.clone();
    ask.push(root.clone());
    let who = people(pool, &ask).await?;

    let root_pub = pubs.iter().find(|p| p.pubkey == root);
    let r = b
        .node("pk", &root, || {
            let mut n = match root_pub {
                Some(p) => publisher_node(p, "root"),
                None => node(
                    &root,
                    "root",
                    pubkey_label(&root, who.get(&root).and_then(|w| w.name.as_deref())),
                    0.0,
                ),
            };
            n.detail.insert(0, "you (trust root)".into());
            n.detail.push(format!(
                "follows {}",
                plural(d1.len(), "account", "accounts")
            ));
            n
        })
        .expect("first node");
    for (k, n) in &via {
        let w = who.get(*k);
        b.node("pk", k, || GraphNode {
            score: w.and_then(|w| w.score).map(|s| s.clamp(0, 100) as u8),
            detail: vec![
                "followed by you".into(),
                format!("follows {} here", plural(*n, "publisher", "publishers")),
            ],
            ..node(
                k,
                "follow",
                pubkey_label(k, w.and_then(|w| w.name.as_deref())),
                *n as f64,
            )
        });
    }
    for p in &pubs {
        b.node("pk", &p.pubkey, || publisher_node(p, "publisher"));
    }
    // The root's own follows first, so they keep their edge from the root.
    for k in via_keys
        .iter()
        .chain(pub_keys.iter().filter(|k| d1_set.contains(k.as_str())))
    {
        if let Some(i) = b.get("pk", k) {
            b.edge(r, i, 1.0, "follows");
        }
    }
    for (f, t) in &edges {
        if let (Some(x), Some(y)) = (b.get("pk", f), b.get("pk", t)) {
            b.edge(x, y, 1.0, "follows");
        }
    }
    let mut g = b.done();
    if d1.is_empty() {
        g.note =
            Some("The trust root's follow list is not known yet (the crawler fetches it).".into());
    }
    Ok(g)
}

#[derive(FromRow)]
struct RelayRow {
    url: String,
    enabled: bool,
    last_ok: Option<i64>,
    events_found: i64,
    consecutive_failures: i32,
}

/// Relays ↔ publishers: where the top publishers say they write (NIP-65 write relays; which
/// relay delivered an event is not recorded), relays sized by the torrents of their publishers.
pub async fn relays(store: &PgStore, now: i64) -> Result<Graph> {
    let pool = store.pool();
    let mut b = Builder::new("relays");
    let pubs = top_publishers(pool, RELAY_PUBLISHERS).await?;
    let keys: Vec<String> = pubs.iter().map(|p| p.pubkey.clone()).collect();
    let lists: Vec<(String, String)> = sqlx::query_as(
        "SELECT pubkey, url FROM relay_lists WHERE pubkey = ANY($1) AND write ORDER BY pubkey, url",
    )
    .bind(&keys)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let torrents: HashMap<&str, i32> = pubs
        .iter()
        .map(|p| (p.pubkey.as_str(), p.torrents))
        .collect();
    let mut by_relay: HashMap<&str, (usize, i64)> = HashMap::new();
    for (pk, url) in &lists {
        let e = by_relay.entry(url.as_str()).or_default();
        e.0 += 1;
        e.1 += i64::from(*torrents.get(pk.as_str()).unwrap_or(&0));
    }
    let with_list: HashSet<&str> = lists.iter().map(|(pk, _)| pk.as_str()).collect();
    let mut relays: Vec<(&str, (usize, i64))> = by_relay.into_iter().collect();
    relays.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let room = MAX_GRAPH_NODES.saturating_sub(with_list.len());
    if relays.len() > room {
        b.g.truncated = true;
        relays.truncate(room);
    }
    let urls: Vec<String> = relays.iter().map(|(u, _)| u.to_string()).collect();
    let known: Vec<RelayRow> = sqlx::query_as(
        "SELECT url, enabled, last_ok, events_found, consecutive_failures FROM relays
         WHERE url = ANY($1)",
    )
    .bind(&urls)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let known: HashMap<&str, &RelayRow> = known.iter().map(|r| (r.url.as_str(), r)).collect();
    for (url, (n, t)) in &relays {
        b.node("relay", url, || {
            let mut detail = vec![format!(
                "{} write here",
                plural(*n, "publisher", "publishers")
            )];
            match known.get(url) {
                Some(r) => {
                    detail.push(format!("{} events found by the crawler", r.events_found));
                    if !r.enabled {
                        detail.push("disabled".into());
                    } else if r.consecutive_failures > 0 {
                        detail.push(format!("failing ({} in a row)", r.consecutive_failures));
                    } else if let Some(ok) = r.last_ok {
                        detail.push(format!("answered {} h ago", (now - ok).max(0) / 3600));
                    }
                }
                None => detail.push("not crawled".into()),
            }
            GraphNode {
                detail,
                ..node(
                    url,
                    "relay",
                    url.trim_start_matches("wss://")
                        .trim_end_matches('/')
                        .to_string(),
                    *t as f64,
                )
            }
        });
    }
    for p in pubs
        .iter()
        .filter(|p| with_list.contains(p.pubkey.as_str()))
    {
        b.node("pk", &p.pubkey, || publisher_node(p, "publisher"));
    }
    for (pk, url) in &lists {
        if let (Some(x), Some(y)) = (b.get("pk", pk), b.get("relay", url)) {
            let w = f64::from(*torrents.get(pk.as_str()).unwrap_or(&0));
            b.edge(x, y, w, "writes_to");
        }
    }
    let mut g = b.done();
    let without = pubs.len() - with_list.len();
    if without > 0 {
        g.note = Some(format!(
            "{} of the top {} publish no relay list and are not shown.",
            without,
            pubs.len()
        ));
    }
    Ok(g)
}

/// One title's releases (D44 groups): the title, its qualities, the most seeded releases, and
/// the languages, release groups and seasons they share. None for an unknown media.
pub async fn media(store: &PgStore, media_id: i64, now: i64) -> Result<Option<Graph>> {
    let q = SearchQuery::default();
    let Some(r) = store.media_releases(media_id, &q, now).await? else {
        return Ok(None);
    };
    let mut b = Builder::new("media");
    let title = r.media.as_ref().map_or_else(String::new, |m| match m.year {
        Some(y) => format!("{} ({y})", m.title),
        None => m.title.clone(),
    });
    let total = r.total;
    let m = b
        .node("media", &media_id.to_string(), || GraphNode {
            detail: vec![plural(total as usize, "release", "releases")],
            ..node(&media_id.to_string(), "media", title, total as f64)
        })
        .expect("first node");
    let mut rels: Vec<(
        &release::ReleaseGroup,
        &magnetar_core::model::TorrentSummary,
    )> = r
        .groups
        .iter()
        .flat_map(|g| g.releases.iter().map(move |h| (g, h)))
        .collect();
    rels.sort_by(|a, b| {
        b.1.seeders
            .cmp(&a.1.seeders)
            .then_with(|| b.1.created_at.cmp(&a.1.created_at))
    });
    if rels.len() > MEDIA_RELEASES {
        b.g.truncated = true;
        rels.truncate(MEDIA_RELEASES);
    }
    for (g, h) in rels {
        let quality = g.resolution.clone().unwrap_or_else(|| "unknown".into());
        let Some(qn) = b.node("quality", &quality, || {
            node(
                &quality,
                "quality",
                release::facet_label("resolution", &quality),
                0.0,
            )
        }) else {
            break;
        };
        b.g.nodes[qn as usize].weight += 1.0;
        b.edge(m, qn, 1.0, "quality");
        let seeders = h.seeders.unwrap_or(0);
        let a = h.attrs.clone().unwrap_or_default();
        let Some(rn) = b.node("release", &h.infohash, || {
            let mut detail = vec![match (h.seeders, h.leechers) {
                (Some(s), Some(l)) => format!("{s} seeders, {l} leechers"),
                (Some(s), None) => format!("{s} seeders"),
                _ => "seeders unknown".into(),
            }];
            if let Some(size) = h.total_size {
                detail.push(format!("{:.1} GiB", size as f64 / (1u64 << 30) as f64));
            }
            detail.push(g.language_label.clone());
            GraphNode {
                detail,
                ..node(&h.infohash, "release", h.title.clone(), f64::from(seeders))
            }
        }) else {
            break;
        };
        b.edge(qn, rn, 1.0, "quality");
        if !g.language.is_empty()
            && let Some(ln) = b.node("language", &g.language, || {
                node(&g.language, "language", g.language_label.clone(), 0.0)
            })
        {
            b.g.nodes[ln as usize].weight += 1.0;
            b.edge(rn, ln, 1.0, "language");
        }
        if let Some(grp) = a.group.as_deref()
            && let Some(gn) = b.node("group", grp, || node(grp, "group", grp.to_string(), 0.0))
        {
            b.g.nodes[gn as usize].weight += 1.0;
            b.edge(rn, gn, 1.0, "group");
        }
        if let Some(s) = g.season
            && let Some(sn) = b.node("season", &s.to_string(), || {
                node(&s.to_string(), "season", format!("Season {s}"), 0.0)
            })
        {
            b.g.nodes[sn as usize].weight += 1.0;
            b.edge(rn, sn, 1.0, "season");
        }
    }
    Ok(Some(b.done()))
}

fn source_label(s: LivenessSource) -> &'static str {
    match s {
        LivenessSource::Udp => "a UDP tracker",
        LivenessSource::Http => "an HTTP tracker",
        LivenessSource::Dht => "the DHT (peer estimate)",
        LivenessSource::DhtValues => "the DHT (peers found)",
        LivenessSource::None => "nobody",
    }
}

/// One torrent and where its peers are counted: its trackers (state of the server's checker),
/// the DHT, and who published it. None for an unknown infohash.
pub async fn swarm(
    store: &PgStore,
    liveness: &LivenessService,
    infohash: &str,
    now: i64,
) -> Result<Option<Graph>> {
    let Some(d) = store.torrent_detail(infohash, now).await? else {
        return Ok(None);
    };
    let mut b = Builder::new("swarm");
    let live = d.liveness.as_ref();
    let s = &d.summary;
    let t = b
        .node("torrent", infohash, || {
            let mut detail = Vec::new();
            match live {
                Some(l) => {
                    if let Some(sd) = l.seeders {
                        detail.push(format!(
                            "{sd} seeders, {} leechers",
                            l.leechers.unwrap_or(0)
                        ));
                    }
                    if let Some(c) = l.completed {
                        detail.push(format!("{c} downloads completed"));
                    }
                    detail.push(format!("best answer from {}", source_label(l.source)));
                    detail.push(format!(
                        "{} trackers answered, {} failed, {} h ago",
                        l.trackers_ok,
                        l.trackers_failed,
                        (now - l.checked_at).max(0) / 3600
                    ));
                }
                None => detail.push("not checked yet".into()),
            }
            GraphNode {
                detail,
                ..node(
                    infohash,
                    "torrent",
                    s.title.clone(),
                    f64::from(live.and_then(|l| l.best_peers()).unwrap_or(0)),
                )
            }
        })
        .expect("first node");
    let (trackers, fallback) = if d.trackers.is_empty() {
        (
            DEFAULT_TRACKERS
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            true,
        )
    } else {
        (d.trackers.clone(), false)
    };
    for url in &trackers {
        let state = liveness.tracker_state(url);
        let Some(n) = b.node("tracker", url, || {
            let mut detail = Vec::new();
            if fallback {
                detail.push("added to its magnet link (the event names no tracker)".into());
            }
            detail.push(match state {
                None => "not asked by this server yet".into(),
                Some((_, true)) => "not answering: the checker is backing off".into(),
                Some((f, false)) if f > 0 => format!("{f} failed scrapes in a row"),
                Some(_) => "answering".into(),
            });
            let host = url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.clone());
            GraphNode {
                detail,
                ..node(
                    url,
                    "tracker",
                    host,
                    if matches!(state, Some((_, true))) {
                        0.0
                    } else {
                        1.0
                    },
                )
            }
        }) else {
            break;
        };
        b.edge(t, n, 1.0, "tracker");
    }
    if let Some(l) = live.filter(|l| {
        l.dht_peers_est.is_some()
            || matches!(l.source, LivenessSource::Dht | LivenessSource::DhtValues)
    }) && let Some(n) = b.node("dht", "dht", || GraphNode {
        detail: vec![match l.dht_peers_est {
            Some(p) => format!("≈ {p} peers (BEP 33 estimate)"),
            None => "asked, no estimate".into(),
        }],
        ..node(
            "dht",
            "dht",
            "DHT".into(),
            f64::from(l.dht_peers_est.unwrap_or(0)),
        )
    }) {
        b.edge(t, n, f64::from(l.dht_peers_est.unwrap_or(0)), "dht");
    }
    let mut seen = HashSet::new();
    let pks: Vec<String> = d
        .events
        .iter()
        .filter(|e| seen.insert(e.pubkey.clone()))
        .map(|e| e.pubkey.clone())
        .collect();
    let who = people(store.pool(), &pks).await?;
    for e in &d.events {
        let w = who.get(&e.pubkey);
        let Some(n) = b.node("pk", &e.pubkey, || GraphNode {
            score: Some(e.trust),
            detail: vec![format!(
                "published it {} days ago",
                (now - e.created_at).max(0) / 86400
            )],
            ..node(
                &e.pubkey,
                "publisher",
                pubkey_label(&e.pubkey, w.and_then(|w| w.name.as_deref())),
                1.0,
            )
        }) else {
            break;
        };
        b.edge(n, t, 1.0, "published");
    }
    Ok(Some(b.done()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_caps_and_dedupes() {
        let mut b = Builder::new("t");
        let a = b
            .node("x", "a", || node("a", "k", "A".into(), 1.0))
            .unwrap();
        assert_eq!(b.node("x", "a", || unreachable!()), Some(a));
        let c = b
            .node("y", "a", || node("a", "k", "A".into(), 1.0))
            .unwrap();
        b.edge(a, c, 1.0, "e");
        b.edge(a, c, 2.0, "e");
        b.edge(a, a, 1.0, "e");
        assert_eq!(b.g.edges.len(), 1);
        for i in 0..MAX_GRAPH_NODES * 2 {
            b.node("z", &i.to_string(), || node("", "k", String::new(), 0.0));
        }
        let g = b.done();
        assert_eq!(g.nodes.len(), MAX_GRAPH_NODES);
        assert!(g.truncated);
    }

    #[test]
    fn labels() {
        assert_eq!(pubkey_label(&"a".repeat(64), Some("  alice ")), "alice");
        assert!(pubkey_label(&"a".repeat(64), None).ends_with('…'));
        let f = |code: &str, delta| TrustFactor {
            code: code.into(),
            delta,
            detail: code.to_lowercase(),
            evidence: vec![],
        };
        assert_eq!(
            factor_lines(&[f("AGE", 10), f("REPORTED", -40)]),
            vec!["-40 reported", "+10 age"]
        );
    }
}
