//! `PgStore`: the Postgres implementation of `core::store::Store` (ARCHITECTURE.md §4-§5).
//!
//! Runtime-checked queries only (D22). Newly stored events are also published on a broadcast
//! channel so `/relay` subscriptions see crawler, batch and relay ingests live.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use magnetar_core::api::{FacetSet, FacetsResponse, MediaReleases, SearchIndexStatus};
use magnetar_core::error::{Error, Result};
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::store::{DEFAULT_TRUST_THRESHOLD, Store, keys};
use magnetar_core::{magnet, release, trust, wot};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use tokio::sync::broadcast;

use crate::filter::{Filter, indexed_tags};
use crate::search;
use crate::search_index::{self, SearchIndex};

/// Default sqlx pool cap (`MAGNETAR_DB_POOL`, ARCHITECTURE.md §9, D37).
pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;
/// Relay retry backoff: 5 min doubling per consecutive failure, capped at 24 h.
pub const BACKOFF_BASE_SECS: i64 = 300;
pub const BACKOFF_MAX_SECS: i64 = 24 * 3600;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    live: broadcast::Sender<RawEvent>,
    /// Tantivy index used by `search` once built (D43); attached by `serve`.
    search_index: Arc<OnceLock<Arc<SearchIndex>>>,
}

pub(crate) fn db(e: sqlx::Error) -> Error {
    Error::Store(e.to_string())
}

/// Postgres text cannot hold NUL; everything else is kept verbatim.
pub(crate) fn clean(s: &str) -> Cow<'_, str> {
    if s.contains('\0') {
        Cow::Owned(s.replace('\0', ""))
    } else {
        Cow::Borrowed(s)
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u32(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// Seconds until the next try after `failures` consecutive failures (1-based).
pub fn backoff_secs(failures: u32) -> i64 {
    let shift = failures.saturating_sub(1).min(20);
    (BACKOFF_BASE_SECS << shift).min(BACKOFF_MAX_SECS)
}

/// NIP-01 replaceable ordering: is (a_ts, a_id) the one to keep over (b_ts, b_id)?
pub fn is_newer(a_ts: i64, a_id: &str, b_ts: i64, b_id: &str) -> bool {
    a_ts > b_ts || (a_ts == b_ts && a_id < b_id)
}

/// Overrides and root mutes pin the score; PoW does not move it (core::trust::event_trust).
fn pinned(factors: &[TrustFactor]) -> bool {
    factors
        .iter()
        .any(|f| matches!(f.code.as_str(), "BLOCKED" | "TRUSTED" | "ROOT_MUTED"))
}

fn event_trust(score: u8, pinned: bool, pow_bits: u8) -> u8 {
    if pinned {
        score
    } else {
        (u16::from(score) + u16::from(trust::pow_bonus(pow_bits))).min(100) as u8
    }
}

/// Setting values are JSON; accept bare strings too.
fn json_string(v: &str) -> Option<String> {
    match serde_json::from_str::<Option<String>>(v) {
        Ok(s) => s.filter(|s| !s.is_empty()),
        Err(_) => Some(v.trim().to_string()).filter(|s| !s.is_empty() && s != "null"),
    }
}

pub(crate) fn json_u8(v: &str) -> Option<u8> {
    let v = v.trim().trim_matches('"');
    v.parse::<u8>().ok()
}

pub(crate) fn liveness_source_str(s: LivenessSource) -> &'static str {
    match s {
        LivenessSource::Udp => "udp",
        LivenessSource::Http => "http",
        LivenessSource::Dht => "dht",
        LivenessSource::DhtValues => "dht_values",
        LivenessSource::None => "none",
    }
}

fn liveness_source(s: &str) -> LivenessSource {
    match s {
        "udp" => LivenessSource::Udp,
        "http" => LivenessSource::Http,
        "dht" => LivenessSource::Dht,
        "dht_values" => LivenessSource::DhtValues,
        _ => LivenessSource::None,
    }
}

fn category(s: &str) -> Category {
    s.parse().unwrap_or(Category::Other)
}

#[derive(FromRow)]
struct RelayRow {
    url: String,
    source: String,
    enabled: bool,
    last_ok: Option<i64>,
    last_err: Option<String>,
    consecutive_failures: i32,
    next_try_at: i64,
    events_found: i64,
    supports_neg: Option<bool>,
}

impl From<RelayRow> for RelayInfo {
    fn from(r: RelayRow) -> Self {
        RelayInfo {
            url: r.url,
            source: r.source,
            enabled: r.enabled,
            last_ok: r.last_ok,
            last_err: r.last_err,
            consecutive_failures: to_u32(r.consecutive_failures),
            next_try_at: r.next_try_at,
            events_found: u64::try_from(r.events_found).unwrap_or(0),
            supports_neg: r.supports_neg,
        }
    }
}

#[derive(FromRow)]
struct LivenessRow {
    infohash: String,
    seeders: Option<i32>,
    leechers: Option<i32>,
    completed: Option<i32>,
    dht_peers_est: Option<i32>,
    source: String,
    trackers_ok: i32,
    trackers_failed: i32,
    checked_at: i64,
    expires_at: i64,
}

impl From<LivenessRow> for Liveness {
    fn from(r: LivenessRow) -> Self {
        let opt = |v: Option<i32>| v.map(to_u32);
        Liveness {
            infohash: r.infohash,
            seeders: opt(r.seeders),
            leechers: opt(r.leechers),
            completed: opt(r.completed),
            dht_peers_est: opt(r.dht_peers_est),
            source: liveness_source(&r.source),
            trackers_ok: u16::try_from(r.trackers_ok).unwrap_or(u16::MAX),
            trackers_failed: u16::try_from(r.trackers_failed).unwrap_or(u16::MAX),
            checked_at: r.checked_at,
            expires_at: r.expires_at,
        }
    }
}

#[derive(FromRow)]
struct TrustRow {
    pubkey: String,
    root: Option<String>,
    score: i16,
    factors: Json<Vec<TrustFactor>>,
    computed_at: i64,
    graph_version: i64,
}

impl From<TrustRow> for TrustScore {
    fn from(r: TrustRow) -> Self {
        TrustScore {
            pubkey: r.pubkey,
            score: r.score.clamp(0, 100) as u8,
            factors: r.factors.0,
            root: r.root,
            computed_at: r.computed_at,
            graph_version: r.graph_version,
        }
    }
}

#[derive(FromRow)]
struct SavedRow {
    id: i64,
    name: String,
    query: Json<SearchQuery>,
    notify: bool,
    last_checked: Option<i64>,
    last_seen_created_at: Option<i64>,
}

impl From<SavedRow> for SavedSearch {
    fn from(r: SavedRow) -> Self {
        SavedSearch {
            id: r.id,
            name: r.name,
            query: r.query.0,
            notify: r.notify,
            last_checked: r.last_checked,
            last_seen_created_at: r.last_seen_created_at,
        }
    }
}

#[derive(FromRow)]
struct DetailRow {
    event_id: String,
    pubkey: String,
    created_at: i64,
    title: String,
    description: String,
    files: Json<Vec<TorrentFile>>,
    file_count: i32,
    total_size: Option<i64>,
    trackers: Vec<String>,
    category: String,
    category_path: Vec<String>,
    hashtags: Vec<String>,
    external_ids: Json<Vec<(String, String)>>,
    source_url: Option<String>,
    pow_bits: i16,
    trust: i16,
}

/// `dht_torrents` as the detail view reads it (D50).
#[derive(FromRow)]
struct DhtDetailRow {
    title: String,
    files: Json<Vec<TorrentFile>>,
    file_count: i32,
    total_size: i64,
    category: String,
    first_seen: i64,
    fetched_at: i64,
    discovered_via: String,
    peers_seen: i32,
    infohash_v2: Option<String>,
}

impl DhtDetailRow {
    fn info(self) -> DhtInfo {
        DhtInfo {
            first_seen: self.first_seen,
            fetched_at: self.fetched_at,
            title: self.title,
            file_count: u32::try_from(self.file_count).unwrap_or(0),
            total_size: u64::try_from(self.total_size).unwrap_or(0),
            category: self.category.parse().ok(),
            peers_seen: u32::try_from(self.peers_seen).unwrap_or(0),
            discovered_via: self.discovered_via,
            infohash_v2: self.infohash_v2,
        }
    }
}

#[derive(FromRow)]
struct CommentRow {
    event_id: String,
    pubkey: String,
    created_at: i64,
    kind: i32,
    torrent_event_id: String,
    content: String,
}

impl From<CommentRow> for Comment {
    fn from(r: CommentRow) -> Self {
        Comment {
            event_id: r.event_id,
            pubkey: r.pubkey,
            created_at: r.created_at,
            kind: u16::try_from(r.kind).unwrap_or(0),
            torrent_event_id: r.torrent_event_id,
            content: r.content,
        }
    }
}

#[derive(FromRow)]
struct StatsRow {
    torrents: i64,
    events: i64,
    publishers: i64,
    comments: i64,
    relays: i64,
    db_bytes: i64,
}

#[derive(FromRow)]
struct PublisherRow {
    first_seen: Option<i64>,
    torrent_count: i64,
    has_profile: bool,
    has_relay_list: bool,
    max_per_hour: i64,
    is_bridge: bool,
}

/// Derived rows of a replaceable event, extracted before the transaction.
enum Derived {
    Profile(Option<String>),
    Contacts(Vec<String>, Vec<wot::RelayListEntry>, bool),
    Mutes(Vec<String>),
    RelayList(Vec<wot::RelayListEntry>, bool),
}

impl PgStore {
    /// Connect with at most `max_connections` pooled connections (`MAGNETAR_DB_POOL`).
    ///
    /// Sessions run with `client_min_messages = warning`: Postgres NOTICEs (thousands of "word
    /// is too long to be indexed" from `to_tsvector` on long file names, which sqlx logs at
    /// INFO) are not sent at all, while WARNINGs and errors still reach the log.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let opts = url
            .parse::<PgConnectOptions>()
            .map_err(db)?
            .options([("client_min_messages", "warning")]);
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(db)?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        let (live, _) = broadcast::channel(1024);
        Self {
            pool,
            live,
            search_index: Arc::default(),
        }
    }

    /// Route searches through `idx` once its first build is complete (D43). Only the first
    /// attached index counts.
    pub fn attach_search_index(&self, idx: Arc<SearchIndex>) {
        let _ = self.search_index.set(idx);
    }

    pub fn search_index(&self) -> Option<&Arc<SearchIndex>> {
        self.search_index.get()
    }

    fn ready_index(&self) -> Option<&Arc<SearchIndex>> {
        self.search_index.get().filter(|i| i.is_ready())
    }

    /// Searches and facets go through the index (D43).
    pub fn index_ready(&self) -> bool {
        self.ready_index().is_some()
    }

    /// Index state for `/api/v1/stats`; None when no index is attached (disabled).
    pub async fn search_index_status(&self) -> Option<SearchIndexStatus> {
        let idx = self.search_index.get()?.clone();
        // `status` lists the index directory: off the runtime.
        let mut st = tokio::task::spawn_blocking(move || idx.status())
            .await
            .ok()?;
        match search_index::load::backlog(&self.pool, magnetar_core::now()).await {
            Ok((pending, lag)) => {
                st.pending = pending;
                st.lag_secs = lag;
            }
            Err(e) => tracing::warn!("search index backlog: {e}"),
        }
        Some(st)
    }

    /// Facet value counts over a search's results (D43, several at once since D44, each
    /// without its own filter). Err(Invalid) for an unknown facet; Ok(None) while the index is
    /// not ready.
    pub async fn facet_set(
        &self,
        q: &SearchQuery,
        fields: &[String],
        size: usize,
        now: i64,
    ) -> Result<Option<FacetSet>> {
        if let Some(bad) = fields.iter().find(|f| {
            !search_index::schema::FACETS
                .iter()
                .any(|d| d.name == f.as_str())
        }) {
            return Err(Error::Invalid(format!("unknown facet {bad:?}")));
        }
        // Each facet once (one count vector per field and segment, whatever the body says).
        let mut uniq: Vec<String> = Vec::with_capacity(fields.len());
        for f in fields {
            if !uniq.contains(f) {
                uniq.push(f.clone());
            }
        }
        let fields = &uniq[..];
        let Some(idx) = self.ready_index() else {
            return Ok(None);
        };
        let min_trust = match q.min_trust {
            Some(t) => t,
            None => self.threshold().await?,
        };
        search_index::facet_set(&self.pool, idx, q, fields, size, min_trust, now)
            .await?
            .map(Some)
            .ok_or_else(|| Error::Invalid("unknown facet".into()))
    }

    /// One facet (`GET /api/v1/facets?field=…`).
    pub async fn facets(
        &self,
        q: &SearchQuery,
        field: &str,
        size: usize,
        now: i64,
    ) -> Result<Option<FacetsResponse>> {
        Ok(self
            .facet_set(q, &[field.to_string()], size, now)
            .await?
            .and_then(|mut s| s.facets.pop()))
    }

    /// Full tallies of index facets `fields` (names as in `schema::FACETS`, each once) for the
    /// D47 breakdowns. Ok(None) while the index is not ready.
    pub async fn facet_tallies(
        &self,
        q: &SearchQuery,
        fields: &[String],
        now: i64,
    ) -> Result<Option<(u64, Vec<search_index::collect::FacetTally>)>> {
        let Some(idx) = self.ready_index() else {
            return Ok(None);
        };
        let min_trust = match q.min_trust {
            Some(t) => t,
            None => self.threshold().await?,
        };
        search_index::facet_tallies(&self.pool, idx, q, fields, min_trust, now)
            .await?
            .map(Some)
            .ok_or_else(|| Error::Invalid("unknown facet".into()))
    }

    /// Releases of one media grouped for the per-title view (D44): every linked torrent
    /// passing `q`'s filters, best seeded first, at most `release::MAX_RELEASES`.
    pub async fn media_releases(
        &self,
        media_id: i64,
        q: &SearchQuery,
        now: i64,
    ) -> Result<Option<MediaReleases>> {
        let Some(media) = crate::enrich::media_by_id(&self.pool, media_id).await? else {
            return Ok(None);
        };
        let mut page_q = q.clone();
        page_q.text = format!("media:{media_id} {}", q.text);
        page_q.sort = Sort::Seeders;
        page_q.descending = true;
        page_q.limit = magnetar_core::query::MAX_LIMIT;
        let mut hits = Vec::new();
        let mut total = 0;
        while hits.len() < release::MAX_RELEASES {
            page_q.offset = hits.len() as u32;
            let page = self.search(&page_q, now).await?;
            total = page.total;
            let n = page.hits.len();
            hits.extend(page.hits);
            if n < page_q.limit as usize {
                break;
            }
        }
        hits.truncate(release::MAX_RELEASES);
        let by_episode = media.kind != MediaKind::Movie;
        let truncated = total > hits.len() as u64;
        Ok(Some(MediaReleases {
            media: Some(media),
            total: total.max(hits.len() as u64),
            truncated,
            groups: release::group_releases(hits, by_episode),
        }))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|e| Error::Store(format!("migrate: {e}")))
    }

    pub async fn ping(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    /// Live feed of newly stored events (any ingest path).
    pub fn subscribe(&self) -> broadcast::Receiver<RawEvent> {
        self.live.subscribe()
    }

    /// Open live receivers (one per `/relay` connection with an open REQ).
    pub fn live_receivers(&self) -> usize {
        self.live.receiver_count()
    }

    fn publish(&self, raw: &RawEvent) {
        // No receivers is fine.
        let _ = self.live.send(raw.clone());
    }

    /// Stored events for one NIP-01 filter, newest first (ties: lowest id), capped at 500.
    pub async fn query_events(&self, f: &Filter) -> Result<Vec<RawEvent>> {
        let limit = f.effective_limit();
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut qb = QueryBuilder::<Postgres>::new("SELECT e.raw FROM events e WHERE TRUE");
        push_filter(&mut qb, f);
        qb.push(" ORDER BY e.created_at DESC, e.id ASC LIMIT ");
        qb.push_bind(limit as i64);
        let rows: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.iter().map(|r| RawEvent::from_json(r)).collect()
    }

    /// Negentropy items (created_at, id) for a NEG-OPEN filter; at most `max` + 1 rows so the
    /// caller can detect an oversized query.
    pub async fn neg_items(&self, f: &Filter, max: usize) -> Result<Vec<(u64, [u8; 32])>> {
        let mut qb =
            QueryBuilder::<Postgres>::new("SELECT e.created_at, e.id FROM events e WHERE TRUE");
        push_filter(&mut qb, f);
        qb.push(" ORDER BY e.created_at, e.id LIMIT ");
        qb.push_bind(max as i64 + 1);
        let rows: Vec<(i64, String)> = qb
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.into_iter()
            .map(|(ts, id)| {
                let mut out = [0u8; 32];
                hex::decode_to_slice(&id, &mut out)
                    .map_err(|_| Error::Store(format!("bad stored id {id}")))?;
                Ok((u64::try_from(ts).unwrap_or(0), out))
            })
            .collect()
    }

    /// A client's edit of a saved search (`PUT /api/v1/saved-searches/{id}`, D77): name, query
    /// and notify only, in one statement, so a notifier run finishing meanwhile keeps its
    /// `last_checked`; `reset` (the query changed) clears `last_seen_created_at`. None = no
    /// such id.
    pub async fn edit_saved_search(
        &self,
        id: i64,
        name: &str,
        query: &SearchQuery,
        notify: bool,
        reset: bool,
    ) -> Result<Option<SavedSearch>> {
        let row: Option<SavedRow> = sqlx::query_as(
            "UPDATE saved_searches SET name = $2, query = $3, notify = $4,
                last_seen_created_at = CASE WHEN $5 THEN NULL ELSE last_seen_created_at END
             WHERE id = $1
             RETURNING id, name, query, notify, last_checked, last_seen_created_at",
        )
        .bind(id)
        .bind(clean(name))
        .bind(Json(query))
        .bind(notify)
        .bind(reset)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(Into::into))
    }

    /// Union of trackers of every event that published this infohash; None if unknown. A
    /// DHT-only torrent (D51) has none: `Some([])`, checked on the default trackers and the DHT
    /// like a tracker-less Nostr torrent (before D77 a forced check of one answered 404).
    pub async fn trackers_for(&self, infohash: &str) -> Result<Option<Vec<String>>> {
        let rows: Vec<Vec<String>> = sqlx::query_scalar(
            "SELECT trackers FROM torrents WHERE infohash = $1 ORDER BY created_at, event_id",
        )
        .bind(infohash)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        if rows.is_empty() {
            let dht: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM dht_torrents WHERE infohash = $1)",
            )
            .bind(infohash)
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
            return Ok(dht.then(Vec::new));
        }
        let mut out: Vec<String> = Vec::new();
        for t in rows.into_iter().flatten() {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        Ok(Some(out))
    }

    /// The DHT row of an infohash (D50), if the crawler stored one.
    async fn dht_row(&self, infohash: &str) -> Result<Option<DhtDetailRow>> {
        sqlx::query_as(
            "SELECT title, files, file_count, total_size, category, first_seen, fetched_at,
                    discovered_via, peers_seen, infohash_v2
             FROM dht_torrents WHERE infohash = $1",
        )
        .bind(infohash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    /// Detail of an infohash only the DHT holds (D51, docs/DHT.md §5): no event, publisher,
    /// description or comments; the magnet gets the default trackers (as a tracker-less Nostr
    /// torrent does).
    async fn dht_detail(&self, infohash: &str, d: DhtDetailRow, now: i64) -> Result<TorrentDetail> {
        let liveness = self.get_liveness(infohash).await?;
        let media = crate::enrich::media_for_infohash(&self.pool, infohash).await?;
        let attrs: Option<Json<release::ReleaseAttrs>> =
            sqlx::query_scalar("SELECT attrs FROM torrent_attrs WHERE infohash = $1")
                .bind(infohash)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        let total_size = u64::try_from(d.total_size).ok();
        let summary = TorrentSummary {
            infohash: infohash.to_string(),
            event_id: String::new(),
            title: d.title.clone(),
            category: category(&d.category),
            total_size,
            file_count: to_u32(d.file_count),
            created_at: d.first_seen,
            pubkey: String::new(),
            npub: String::new(),
            seeders: liveness.as_ref().and_then(|l| l.seeders),
            leechers: liveness.as_ref().and_then(|l| l.leechers),
            dead: liveness.as_ref().and_then(|l| search::dead(l, now)),
            trust: 0,
            pow_bits: 0,
            publisher_count: 0,
            source_url: None,
            rank: 0.0,
            pinned: false,
            hidden: false,
            media_title: media.as_ref().map(|m| m.title.clone()),
            media_year: media.as_ref().and_then(|m| m.year),
            media_kind: media.as_ref().map(|m| m.kind),
            matched_file: None,
            media_id: media.as_ref().map(|m| m.media_id),
            media_rating: media.as_ref().and_then(|m| m.rating),
            attrs: attrs.map(|a| a.0),
            origin: Origin::Dht,
            best_peers: liveness.as_ref().and_then(|l| l.best_peers()),
        };
        let magnet = magnet::build_magnet(infohash, Some(&d.title), &[], total_size);
        let files = d.files.0.clone();
        Ok(TorrentDetail {
            summary,
            description: String::new(),
            files,
            trackers: Vec::new(),
            hashtags: Vec::new(),
            category_path: Vec::new(),
            external_ids: Vec::new(),
            magnet,
            liveness,
            trust: TrustScore {
                pubkey: String::new(),
                score: 0,
                factors: vec![TrustFactor {
                    code: "dht".into(),
                    delta: 0,
                    detail: "Found on the DHT: name and files verified against the infohash; \
                             no publisher"
                        .into(),
                    evidence: Vec::new(),
                }],
                root: None,
                computed_at: 0,
                graph_version: 0,
            },
            events: Vec::new(),
            comments: Vec::new(),
            media,
            dht: Some(d.info()),
        })
    }

    /// The SQL fallback knows each hit's canonical row only: mark hits known from both sources
    /// (an unblocked Nostr event and a DHT row) as `Both`, as the index does (D51).
    async fn refine_origins(&self, hits: &mut [TorrentSummary]) -> Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let ihs: Vec<String> = hits.iter().map(|h| h.infohash.clone()).collect();
        let dht: Vec<String> =
            sqlx::query_scalar("SELECT infohash FROM dht_torrents WHERE infohash = ANY($1)")
                .bind(&ihs)
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        let nostr: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT infohash FROM torrents t WHERE infohash = ANY($1) AND NOT EXISTS (
                 SELECT 1 FROM local_overrides o WHERE o.pubkey = t.pubkey AND o.action = 'block')",
        )
        .bind(&ihs)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        for h in hits {
            h.origin = Origin::from_sources(nostr.contains(&h.infohash), dht.contains(&h.infohash));
        }
        Ok(())
    }

    async fn threshold(&self) -> Result<u8> {
        Ok(self
            .get_setting(keys::TRUST_THRESHOLD)
            .await?
            .and_then(|v| json_u8(&v))
            .unwrap_or(DEFAULT_TRUST_THRESHOLD))
    }

    async fn root(conn: &mut PgConnection) -> Result<Option<String>> {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
            .bind(keys::TRUST_ROOT)
            .fetch_optional(conn)
            .await
            .map_err(db)?;
        Ok(v.and_then(|v| json_string(&v)))
    }
}

/// `AND ...` clauses for a filter on `events e`.
fn push_filter(qb: &mut QueryBuilder<Postgres>, f: &Filter) {
    if let Some(ids) = &f.ids {
        qb.push(" AND e.id = ANY(").push_bind(ids.clone()).push(")");
    }
    if let Some(authors) = &f.authors {
        qb.push(" AND e.pubkey = ANY(")
            .push_bind(authors.clone())
            .push(")");
    }
    if let Some(kinds) = &f.kinds {
        let kinds: Vec<i32> = kinds.iter().map(|&k| i32::from(k)).collect();
        qb.push(" AND e.kind = ANY(").push_bind(kinds).push(")");
    }
    if let Some(s) = f.since {
        qb.push(" AND e.created_at >= ").push_bind(s);
    }
    if let Some(u) = f.until {
        qb.push(" AND e.created_at <= ").push_bind(u);
    }
    for (letter, values) in &f.tags {
        qb.push(" AND EXISTS (SELECT 1 FROM event_tags g WHERE g.event_id = e.id AND g.name = ")
            .push_bind(letter.to_string())
            .push(" AND g.value = ANY(")
            .push_bind(values.clone())
            .push("))");
    }
}

/// Insert into `events` (+ tag index). false if already stored.
async fn insert_event(conn: &mut PgConnection, raw: &RawEvent) -> Result<bool> {
    if raw.created_at < 0 {
        return Err(Error::Invalid("negative created_at".into()));
    }
    let res = sqlx::query(
        "INSERT INTO events (id, pubkey, kind, created_at, raw, received_at)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&raw.id)
    .bind(&raw.pubkey)
    .bind(i32::from(raw.kind))
    .bind(raw.created_at)
    .bind(raw.to_json())
    .bind(magnetar_core::now())
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    if res.rows_affected() == 0 {
        return Ok(false);
    }
    let (names, values): (Vec<String>, Vec<String>) = indexed_tags(raw).into_iter().unzip();
    if !names.is_empty() {
        sqlx::query(
            "INSERT INTO event_tags (event_id, name, value)
             SELECT $1, n, v FROM unnest($2::text[], $3::text[]) AS t(n, v)
             ON CONFLICT DO NOTHING",
        )
        .bind(&raw.id)
        .bind(names)
        .bind(values)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    }
    Ok(true)
}

async fn bump(conn: &mut PgConnection) -> Result<i64> {
    sqlx::query_scalar(
        "UPDATE meta SET value = value + 1 WHERE key = 'graph_version' RETURNING value",
    )
    .fetch_one(conn)
    .await
    .map_err(db)
}

async fn replace_relay_list(
    conn: &mut PgConnection,
    pubkey: &str,
    entries: &[wot::RelayListEntry],
    oversized: bool,
    source: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM relay_lists WHERE pubkey = $1")
        .bind(pubkey)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    let urls: Vec<String> = entries.iter().map(|e| clean(&e.url).into_owned()).collect();
    let reads: Vec<bool> = entries.iter().map(|e| e.read).collect();
    let writes: Vec<bool> = entries.iter().map(|e| e.write).collect();
    sqlx::query(
        "INSERT INTO relay_lists (pubkey, url, read, write)
         SELECT $1, u, bool_or(r), bool_or(w) FROM unnest($2::text[], $3::bool[], $4::bool[]) AS t(u, r, w)
         GROUP BY u",
    )
    .bind(pubkey)
    .bind(urls)
    .bind(reads)
    .bind(writes)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO relay_list_meta (pubkey, oversized, source) VALUES ($1, $2, $3)
         ON CONFLICT (pubkey) DO UPDATE SET oversized = EXCLUDED.oversized, source = EXCLUDED.source",
    )
    .bind(pubkey)
    .bind(oversized)
    .bind(source)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    Ok(())
}

#[async_trait::async_trait]
impl Store for PgStore {
    async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        if !insert_event(&mut tx, raw).await? {
            return Ok(false);
        }
        let cached: Option<(i16, Json<Vec<TrustFactor>>)> =
            sqlx::query_as("SELECT score, factors FROM trust_cache WHERE pubkey = $1")
                .bind(&t.pubkey)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let trust = match &cached {
            Some((score, factors)) => {
                event_trust((*score).clamp(0, 100) as u8, pinned(&factors.0), t.pow_bits)
            }
            None => event_trust(0, false, t.pow_bits),
        };
        let files: Vec<TorrentFile> = t
            .files
            .iter()
            .map(|f| TorrentFile {
                path: clean(&f.path).into_owned(),
                size: f.size,
            })
            .collect();
        let external: Vec<(String, String)> = t
            .external_ids
            .iter()
            .map(|(k, v)| (clean(k).into_owned(), clean(v).into_owned()))
            .collect();
        let strings = |v: &[String]| v.iter().map(|s| clean(s).into_owned()).collect::<Vec<_>>();
        sqlx::query(
            "INSERT INTO torrents (event_id, pubkey, created_at, infohash, infohash_v2, title,
                description, files, file_count, files_text, total_size, trackers, category,
                category_path, hashtags, external_ids, source_url, published_at, pow_bits,
                pow_committed, trust)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                $18, $19, $20, $21)",
        )
        .bind(&t.event_id)
        .bind(&t.pubkey)
        .bind(t.created_at)
        .bind(&t.infohash)
        .bind(&t.infohash_v2)
        .bind(clean(&t.title))
        .bind(clean(&t.description))
        .bind(Json(&files))
        .bind(i32::try_from(t.files.len()).unwrap_or(i32::MAX))
        .bind(clean(&t.files_text()))
        .bind(t.total_size.map(to_i64))
        .bind(strings(&t.trackers))
        .bind(t.category.as_str())
        .bind(strings(&t.category_path))
        .bind(strings(&t.hashtags))
        .bind(Json(&external))
        .bind(t.source_url.as_deref().map(clean))
        .bind(t.published_at)
        .bind(i16::from(t.pow_bits))
        .bind(t.pow_committed.map(i16::from))
        .bind(i16::from(trust))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        // Release attributes (D44), parsed once per infohash: its first event names them (same
        // files; titles of other publishers differ little). Microseconds of work.
        let attrs = release::parse_release(&clean(&t.title), &files, t.category);
        sqlx::query(
            "INSERT INTO torrent_attrs (infohash, parser_version, attrs) VALUES ($1, $2, $3)
             ON CONFLICT (infohash) DO UPDATE
                 SET parser_version = EXCLUDED.parser_version, attrs = EXCLUDED.attrs
                 WHERE torrent_attrs.parser_version < EXCLUDED.parser_version",
        )
        .bind(&t.infohash)
        .bind(release::PARSER_VERSION)
        .bind(Json(&attrs))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        // A new event may carry new links: let the enricher look at the infohash again (D40).
        sqlx::query("DELETE FROM enrich_checked WHERE infohash = $1")
            .bind(&t.infohash)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.publish(raw);
        Ok(true)
    }

    async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        if !insert_event(&mut tx, raw).await? {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO comments (event_id, pubkey, created_at, kind, torrent_event_id, content)
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(&c.event_id)
        .bind(&c.pubkey)
        .bind(c.created_at)
        .bind(i32::from(c.kind))
        .bind(&c.torrent_event_id)
        .bind(clean(&c.content))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.publish(raw);
        Ok(true)
    }

    async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool> {
        let d = if raw.kind == KIND_KIND_MUTE_SET {
            raw.tag("d")
                .and_then(|t| t.get(1))
                .cloned()
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let head: Option<(i64, String)> = sqlx::query_as(
            "SELECT created_at, event_id FROM replaceable_heads
             WHERE pubkey = $1 AND kind = $2 AND d = $3 FOR UPDATE",
        )
        .bind(&raw.pubkey)
        .bind(i32::from(raw.kind))
        .bind(&d)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some((ts, id)) = &head
            && !is_newer(raw.created_at, &raw.id, *ts, id)
        {
            return Ok(false);
        }
        let derived = match raw.kind {
            KIND_PROFILE => Derived::Profile(wot::profile_name(raw)),
            KIND_CONTACTS => {
                let (relays, oversized) = wot::relay_list_of(raw);
                Derived::Contacts(wot::follows_of(raw), relays, oversized)
            }
            KIND_MUTE_LIST | KIND_KIND_MUTE_SET => Derived::Mutes(wot::mutes_of(raw)),
            KIND_RELAY_LIST => {
                let (relays, oversized) = wot::relay_list_of(raw);
                Derived::RelayList(relays, oversized)
            }
            k => return Err(Error::Invalid(format!("kind {k} is not replaceable here"))),
        };
        let updated = sqlx::query(
            "INSERT INTO replaceable_heads (pubkey, kind, d, created_at, event_id)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (pubkey, kind, d) DO UPDATE
               SET created_at = EXCLUDED.created_at, event_id = EXCLUDED.event_id
               WHERE EXCLUDED.created_at > replaceable_heads.created_at
                  OR (EXCLUDED.created_at = replaceable_heads.created_at
                      AND EXCLUDED.event_id < replaceable_heads.event_id)",
        )
        .bind(&raw.pubkey)
        .bind(i32::from(raw.kind))
        .bind(&d)
        .bind(raw.created_at)
        .bind(&raw.id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if updated.rows_affected() == 0 {
            return Ok(false);
        }
        insert_event(&mut tx, raw).await?;
        if let Some((_, old)) = &head
            && old != &raw.id
        {
            sqlx::query("DELETE FROM events WHERE id = $1")
                .bind(old)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }

        let pk = raw.pubkey.as_str();
        match &derived {
            Derived::Profile(name) => {
                sqlx::query(
                    "INSERT INTO profiles (pubkey, name, created_at) VALUES ($1, $2, $3)
                     ON CONFLICT (pubkey) DO UPDATE SET name = EXCLUDED.name, created_at = EXCLUDED.created_at",
                )
                .bind(pk)
                .bind(name.as_deref().map(clean))
                .bind(raw.created_at)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            Derived::Contacts(follows, relays, oversized) => {
                sqlx::query("DELETE FROM follows WHERE follower = $1")
                    .bind(pk)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query(
                    "INSERT INTO follows (follower, followee)
                     SELECT $1, f FROM unnest($2::text[]) AS f ON CONFLICT DO NOTHING",
                )
                .bind(pk)
                .bind(follows)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
                // Legacy kind-3 relay map only when no NIP-65 list is known (D10).
                if !relays.is_empty() {
                    let src: Option<String> =
                        sqlx::query_scalar("SELECT source FROM relay_list_meta WHERE pubkey = $1")
                            .bind(pk)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(db)?;
                    if src.as_deref() != Some("nip65") {
                        replace_relay_list(&mut tx, pk, relays, *oversized, "kind3_content")
                            .await?;
                    }
                }
            }
            Derived::Mutes(targets) => {
                sqlx::query("DELETE FROM mutes WHERE muter = $1 AND source_kind = $2")
                    .bind(pk)
                    .bind(i32::from(raw.kind))
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query(
                    "INSERT INTO mutes (muter, target, source_kind)
                     SELECT $1, t, $3 FROM unnest($2::text[]) AS t ON CONFLICT DO NOTHING",
                )
                .bind(pk)
                .bind(targets)
                .bind(i32::from(raw.kind))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            Derived::RelayList(relays, oversized) => {
                replace_relay_list(&mut tx, pk, relays, *oversized, "nip65").await?;
            }
        }

        if matches!(
            raw.kind,
            KIND_CONTACTS | KIND_MUTE_LIST | KIND_KIND_MUTE_SET
        ) && let Some(root) = Self::root(&mut tx).await?
        {
            let in_graph = root == raw.pubkey || {
                sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM follows WHERE follower = $1 AND followee = $2)",
                )
                .bind(&root)
                .bind(pk)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?
            };
            if in_graph {
                bump(&mut tx).await?;
            }
        }
        tx.commit().await.map_err(db)?;
        self.publish(raw);
        Ok(true)
    }

    async fn put_report(&self, raw: &RawEvent) -> Result<bool> {
        let report = wot::report_of(raw);
        let mut tx = self.pool.begin().await.map_err(db)?;
        if !insert_event(&mut tx, raw).await? {
            return Ok(false);
        }
        if let Some((target, kind)) = report {
            sqlx::query(
                "INSERT INTO reports (event_id, reporter, target, report_type, created_at)
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            )
            .bind(&raw.id)
            .bind(&raw.pubkey)
            .bind(target)
            .bind(clean(&kind))
            .bind(raw.created_at)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        self.publish(raw);
        Ok(true)
    }

    async fn has_event(&self, id: &str) -> Result<bool> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM events WHERE id = $1)")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(db)
    }

    async fn put_blocked(&self, infohash: &str, event_id: &str, rule_version: i16) -> Result<()> {
        // Hash and id only (D53): the event's text is never stored or logged.
        let now = magnetar_core::now();
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO blocked_infohashes (infohash, blocked_at, rule_version, origin)
             VALUES ($1, $2, $3, 'nostr') ON CONFLICT (infohash) DO NOTHING",
        )
        .bind(infohash)
        .bind(now)
        .bind(rule_version)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO blocked_events (id, blocked_at) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(event_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        // A DHT row under the same hash goes too (its name passed the screen without the
        // publisher's adult category or description).
        sqlx::query("DELETE FROM dht_torrents WHERE infohash = $1")
            .bind(infohash)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        // Release attributes parsed from that DHT name, unless a stored event owns them.
        sqlx::query(
            "DELETE FROM torrent_attrs WHERE infohash = $1
             AND NOT EXISTS (SELECT 1 FROM torrents t WHERE t.infohash = $1)",
        )
        .bind(infohash)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    async fn is_blocked(&self, infohash: &str, event_id: &str) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM blocked_infohashes WHERE infohash = $1)
                 OR EXISTS (SELECT 1 FROM blocked_events WHERE id = $2)",
        )
        .bind(infohash)
        .bind(event_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn known_torrent_event(&self, id: &str) -> Result<bool> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM torrents WHERE event_id = $1)")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(db)
    }

    async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT raw FROM events WHERE id = ANY($1) ORDER BY created_at DESC, id",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(|r| RawEvent::from_json(r)).collect()
    }

    async fn upsert_relay(&self, url: &str, source: &str) -> Result<()> {
        // An explicit manual add promotes the source; nothing else touches an existing row.
        sqlx::query(
            "INSERT INTO relays (url, source) VALUES ($1, $2)
             ON CONFLICT (url) DO UPDATE SET source = EXCLUDED.source
               WHERE EXCLUDED.source = 'manual' AND relays.source <> 'manual'",
        )
        .bind(url)
        .bind(source)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn list_relays(&self) -> Result<Vec<RelayInfo>> {
        let rows: Vec<RelayRow> = sqlx::query_as(
            "SELECT url, source, enabled, last_ok, last_err, consecutive_failures, next_try_at,
                    events_found, supports_neg
             FROM relays ORDER BY events_found DESC, url",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn record_relay_torrents(&self, url: &str, torrents: u64) -> Result<()> {
        sqlx::query("UPDATE relays SET torrents_found = torrents_found + $2 WHERE url = $1")
            .bind(url)
            .bind(to_i64(torrents))
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn record_relay_result(
        &self,
        url: &str,
        ok: bool,
        err: Option<&str>,
        found: u64,
        now: i64,
    ) -> Result<()> {
        let found = to_i64(found);
        if ok {
            sqlx::query(
                "UPDATE relays SET consecutive_failures = 0, last_ok = $2, last_err = NULL,
                    next_try_at = $2, events_found = events_found + $3
                 WHERE url = $1",
            )
            .bind(url)
            .bind(now)
            .bind(found)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        } else {
            let mut tx = self.pool.begin().await.map_err(db)?;
            let failures: Option<i32> = sqlx::query_scalar(
                "UPDATE relays SET consecutive_failures = consecutive_failures + 1, last_err = $2,
                    events_found = events_found + $3
                 WHERE url = $1 RETURNING consecutive_failures",
            )
            .bind(url)
            .bind(err.map(clean))
            .bind(found)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
            if let Some(f) = failures {
                sqlx::query("UPDATE relays SET next_try_at = $2 WHERE url = $1")
                    .bind(url)
                    .bind(now + backoff_secs(to_u32(f)))
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            tx.commit().await.map_err(db)?;
        }
        Ok(())
    }

    async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE relays SET enabled = $2 WHERE url = $1")
            .bind(url)
            .bind(enabled)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn set_relay_supports_neg(&self, url: &str, supports: bool) -> Result<()> {
        sqlx::query("UPDATE relays SET supports_neg = $2 WHERE url = $1")
            .bind(url)
            .bind(supports)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>> {
        let row: Option<(Option<i64>, Option<i64>, bool)> = sqlx::query_as(
            "SELECT newest_seen, backfill_until, backfill_done FROM cursors
             WHERE relay = $1 AND kind = $2",
        )
        .bind(relay)
        .bind(i32::from(kind))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(
            row.map(|(newest_seen, backfill_until, backfill_done)| Cursor {
                relay: relay.to_string(),
                kind,
                newest_seen,
                backfill_until,
                backfill_done,
            }),
        )
    }

    async fn put_cursor(&self, c: &Cursor) -> Result<()> {
        sqlx::query(
            "INSERT INTO cursors (relay, kind, newest_seen, backfill_until, backfill_done)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (relay, kind) DO UPDATE SET newest_seen = EXCLUDED.newest_seen,
               backfill_until = EXCLUDED.backfill_until, backfill_done = EXCLUDED.backfill_done",
        )
        .bind(&c.relay)
        .bind(i32::from(c.kind))
        .bind(c.newest_seen)
        .bind(c.backfill_until)
        .bind(c.backfill_done)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn publishers(&self, limit: usize) -> Result<Vec<String>> {
        // Trigger-maintained (D43): the GROUP BY over torrents read the whole wide table.
        sqlx::query_scalar(
            "SELECT pubkey FROM publisher_counts ORDER BY torrents DESC, pubkey LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn relay_lists_for(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<(String, String, bool, bool)>> {
        sqlx::query_as(
            "SELECT r.pubkey, r.url, r.read, r.write FROM relay_lists r
             JOIN relay_list_meta m ON m.pubkey = r.pubkey
             WHERE r.pubkey = ANY($1) AND NOT m.oversized ORDER BY r.pubkey, r.url",
        )
        .bind(pubkeys)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT coalesce((SELECT oversized FROM relay_list_meta WHERE pubkey = $1), false)",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn search(&self, q: &SearchQuery, now: i64) -> Result<SearchPage> {
        let min_trust = match q.min_trust {
            Some(t) => t,
            None => self.threshold().await?,
        };
        if let Some(idx) = self.ready_index() {
            match search_index::search(&self.pool, idx, q, min_trust, now).await {
                Ok(page) => return Ok(page),
                Err(e) => tracing::warn!("search index: {e}; using Postgres"),
            }
        }
        // Postgres fallback (index disabled or still building): the count stops at a cap.
        let mut count = search::count_query(q, now, min_trust);
        let counted: i64 = count
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
        let lower_bound = counted > search::COUNT_CAP;
        let total = counted.min(search::COUNT_CAP);
        let mut rows = search::hits_query(q, now, min_trust);
        let hits: Vec<search::HitRow> = rows
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        let mut hits: Vec<TorrentSummary> = hits.into_iter().map(|h| h.into_summary(now)).collect();
        self.refine_origins(&mut hits).await?;
        Ok(SearchPage {
            hits,
            total: u64::try_from(total).unwrap_or(0),
            source: "server".into(),
            total_is_lower_bound: lower_bound,
        })
    }

    async fn torrent_detail(&self, infohash: &str, now: i64) -> Result<Option<TorrentDetail>> {
        let dht = self.dht_row(infohash).await?;
        let rows: Vec<DetailRow> = sqlx::query_as(
            "SELECT event_id, pubkey, created_at, title, description, files, file_count, total_size,
                    trackers, category, category_path, hashtags, external_ids, source_url,
                    pow_bits, trust
             FROM torrents t
             WHERE infohash = $1 AND NOT EXISTS (
                 SELECT 1 FROM local_overrides o WHERE o.pubkey = t.pubkey AND o.action = 'block')
             ORDER BY trust DESC, created_at ASC, event_id ASC",
        )
        .bind(infohash)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let Some(canon) = rows.first() else {
            // No unblocked Nostr event: the DHT row alone, when there is one (D51).
            return match dht {
                Some(d) => self.dht_detail(infohash, d, now).await.map(Some),
                None => Ok(None),
            };
        };
        let ids: Vec<String> = rows.iter().map(|r| r.event_id.clone()).collect();
        let comments: Vec<CommentRow> = sqlx::query_as(
            "SELECT event_id, pubkey, created_at, kind, torrent_event_id, content FROM comments c
             WHERE torrent_event_id = ANY($1) AND NOT EXISTS (
                 SELECT 1 FROM local_overrides o WHERE o.pubkey = c.pubkey AND o.action = 'block')
             ORDER BY created_at, event_id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let liveness = self.get_liveness(infohash).await?;
        let trust = self
            .get_trust(&canon.pubkey)
            .await?
            .unwrap_or_else(|| TrustScore {
                pubkey: canon.pubkey.clone(),
                score: 0,
                factors: Vec::new(),
                root: None,
                computed_at: 0,
                graph_version: 0,
            });
        let mut trackers: Vec<String> = Vec::new();
        for t in rows.iter().flat_map(|r| r.trackers.iter()) {
            if !trackers.contains(t) {
                trackers.push(t.clone());
            }
        }
        let mut publishers: Vec<&str> = rows.iter().map(|r| r.pubkey.as_str()).collect();
        publishers.sort_unstable();
        publishers.dedup();
        let total_size = canon.total_size.and_then(|s| u64::try_from(s).ok());
        let media = crate::enrich::media_for_infohash(&self.pool, infohash).await?;
        let attrs: Option<Json<release::ReleaseAttrs>> =
            sqlx::query_scalar("SELECT attrs FROM torrent_attrs WHERE infohash = $1")
                .bind(infohash)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        let summary = TorrentSummary {
            infohash: infohash.to_string(),
            event_id: canon.event_id.clone(),
            title: canon.title.clone(),
            category: category(&canon.category),
            total_size,
            file_count: to_u32(canon.file_count),
            created_at: canon.created_at,
            pubkey: canon.pubkey.clone(),
            npub: npub(&canon.pubkey),
            seeders: liveness.as_ref().and_then(|l| l.seeders),
            leechers: liveness.as_ref().and_then(|l| l.leechers),
            dead: liveness.as_ref().and_then(|l| search::dead(l, now)),
            trust: canon.trust.clamp(0, 100) as u8,
            pow_bits: canon.pow_bits.clamp(0, 255) as u8,
            publisher_count: publishers.len() as u32,
            source_url: canon.source_url.clone(),
            rank: 0.0,
            pinned: false,
            hidden: false,
            media_title: media.as_ref().map(|m| m.title.clone()),
            media_year: media.as_ref().and_then(|m| m.year),
            media_kind: media.as_ref().map(|m| m.kind),
            matched_file: None,
            media_id: media.as_ref().map(|m| m.media_id),
            media_rating: media.as_ref().and_then(|m| m.rating),
            attrs: attrs.map(|a| a.0),
            origin: Origin::from_sources(true, dht.is_some()),
            best_peers: liveness.as_ref().and_then(|l| l.best_peers()),
        };
        let events = rows
            .iter()
            .map(|r| EventRef {
                event_id: r.event_id.clone(),
                pubkey: r.pubkey.clone(),
                npub: npub(&r.pubkey),
                created_at: r.created_at,
                title: r.title.clone(),
                trust: r.trust.clamp(0, 100) as u8,
            })
            .collect();
        let magnet = magnet::build_magnet(infohash, Some(&canon.title), &trackers, total_size);
        Ok(Some(TorrentDetail {
            summary,
            description: canon.description.clone(),
            files: canon.files.0.clone(),
            trackers,
            hashtags: canon.hashtags.clone(),
            category_path: canon.category_path.clone(),
            external_ids: canon.external_ids.0.clone(),
            magnet,
            liveness,
            trust,
            events,
            comments: comments.into_iter().map(Into::into).collect(),
            media,
            dht: dht.map(DhtDetailRow::info),
        }))
    }

    async fn liveness_due(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>> {
        // The scheduler's lanes and due order (D45).
        Ok(crate::liveness::due(&self.pool, now, limit)
            .await?
            .into_iter()
            .map(|r| (r.infohash, r.trackers))
            .collect())
    }

    async fn put_liveness(&self, l: &Liveness) -> Result<()> {
        crate::liveness::put_one(&self.pool, l, crate::liveness::Expiry::AsGiven).await
    }

    async fn get_liveness(&self, infohash: &str) -> Result<Option<Liveness>> {
        let row: Option<LivenessRow> = sqlx::query_as(
            "SELECT infohash, seeders, leechers, completed, dht_peers_est, source, trackers_ok,
                    trackers_failed, checked_at, expires_at
             FROM liveness WHERE infohash = $1",
        )
        .bind(infohash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(Into::into))
    }

    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT followee FROM follows WHERE follower = $1 ORDER BY followee")
            .bind(pubkey)
            .fetch_all(&self.pool)
            .await
            .map_err(db)
    }

    async fn followers_among(
        &self,
        targets: &[String],
        among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>> {
        sqlx::query_as(
            "SELECT followee, array_agg(follower ORDER BY follower) FROM follows
             WHERE followee = ANY($1) AND follower = ANY($2)
             GROUP BY followee ORDER BY followee",
        )
        .bind(targets)
        .bind(among)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>> {
        sqlx::query_as(
            "SELECT DISTINCT muter, target FROM mutes WHERE muter = ANY($1) ORDER BY muter, target",
        )
        .bind(muters)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>> {
        sqlx::query_as(
            "SELECT reporter, target, report_type FROM reports WHERE reporter = ANY($1)
             ORDER BY created_at, event_id",
        )
        .bind(reporters)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn comment_authors_on(&self, publisher: &str, authors: &[String]) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT DISTINCT c.pubkey FROM comments c
             JOIN torrents t ON t.event_id = c.torrent_event_id
             WHERE t.pubkey = $1 AND c.pubkey = ANY($2) ORDER BY c.pubkey",
        )
        .bind(publisher)
        .bind(authors)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats> {
        let r: PublisherRow = sqlx::query_as(
            "SELECT
               (SELECT min(created_at) FROM events WHERE pubkey = $1) AS first_seen,
               (SELECT count(*) FROM torrents WHERE pubkey = $1) AS torrent_count,
               EXISTS (SELECT 1 FROM profiles WHERE pubkey = $1) AS has_profile,
               EXISTS (SELECT 1 FROM relay_list_meta WHERE pubkey = $1 AND source = 'nip65')
                 AS has_relay_list,
               coalesce((SELECT max(c) FROM (
                   SELECT count(*) OVER (ORDER BY created_at
                       RANGE BETWEEN CURRENT ROW AND 3599 FOLLOWING) AS c
                   FROM torrents WHERE pubkey = $1) w), 0) AS max_per_hour,
               EXISTS (SELECT 1 FROM torrents WHERE pubkey = $1 AND source_url IS NOT NULL)
                 AS is_bridge",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(PublisherStats {
            first_seen: r.first_seen,
            torrent_count: u64::try_from(r.torrent_count).unwrap_or(0),
            has_profile: r.has_profile,
            has_relay_list: r.has_relay_list,
            max_per_hour: u32::try_from(r.max_per_hour).unwrap_or(u32::MAX),
            is_bridge: r.is_bridge,
            wot_comments: 0,
        })
    }

    async fn put_trust(&self, s: &TrustScore) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO trust_cache (pubkey, root, score, factors, computed_at, graph_version)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (pubkey) DO UPDATE SET root = EXCLUDED.root, score = EXCLUDED.score,
               factors = EXCLUDED.factors, computed_at = EXCLUDED.computed_at,
               graph_version = EXCLUDED.graph_version",
        )
        .bind(&s.pubkey)
        .bind(&s.root)
        .bind(i16::from(s.score.min(100)))
        .bind(Json(&s.factors))
        .bind(s.computed_at)
        .bind(s.graph_version)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let bits: Vec<i16> =
            sqlx::query_scalar("SELECT DISTINCT pow_bits FROM torrents WHERE pubkey = $1")
                .bind(&s.pubkey)
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
        let pin = pinned(&s.factors);
        let trusts: Vec<i16> = bits
            .iter()
            .map(|&b| i16::from(event_trust(s.score.min(100), pin, b.clamp(0, 255) as u8)))
            .collect();
        sqlx::query(
            "UPDATE torrents t SET trust = m.trust
             FROM unnest($2::smallint[], $3::smallint[]) AS m(bits, trust)
             WHERE t.pubkey = $1 AND t.pow_bits = m.bits AND t.trust <> m.trust",
        )
        .bind(&s.pubkey)
        .bind(bits)
        .bind(trusts)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>> {
        let row: Option<TrustRow> = sqlx::query_as(
            "SELECT pubkey, root, score, factors, computed_at, graph_version
             FROM trust_cache WHERE pubkey = $1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(Into::into))
    }

    async fn graph_version(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'graph_version'")
            .fetch_one(&self.pool)
            .await
            .map_err(db)
    }

    async fn bump_graph_version(&self) -> Result<i64> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        bump(&mut conn).await
    }

    async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()> {
        match action {
            Some(a) => {
                let a = match a {
                    OverrideAction::Block => "block",
                    OverrideAction::Trust => "trust",
                };
                sqlx::query(
                    "INSERT INTO local_overrides (pubkey, action, created_at) VALUES ($1, $2, $3)
                     ON CONFLICT (pubkey) DO UPDATE SET action = EXCLUDED.action,
                       created_at = EXCLUDED.created_at",
                )
                .bind(pubkey)
                .bind(a)
                .bind(magnetar_core::now())
                .execute(&self.pool)
                .await
                .map_err(db)?;
            }
            None => {
                sqlx::query("DELETE FROM local_overrides WHERE pubkey = $1")
                    .bind(pubkey)
                    .execute(&self.pool)
                    .await
                    .map_err(db)?;
            }
        }
        Ok(())
    }

    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT pubkey, action FROM local_overrides ORDER BY pubkey")
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|(pk, a)| {
                let a = if a == "trust" {
                    OverrideAction::Trust
                } else {
                    OverrideAction::Block
                };
                (pk, a)
            })
            .collect())
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 FOR UPDATE")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(key)
        .bind(clean(value))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        // A new root invalidates every cached score.
        if key == keys::TRUST_ROOT && old.as_deref().and_then(json_string) != json_string(value) {
            bump(&mut tx).await?;
        }
        tx.commit().await.map_err(db)
    }

    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
        let rows: Vec<SavedRow> = sqlx::query_as(
            "SELECT id, name, query, notify, last_checked, last_seen_created_at
             FROM saved_searches ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64> {
        if s.id == 0 {
            return sqlx::query_scalar(
                "INSERT INTO saved_searches (name, query, notify, last_checked, last_seen_created_at)
                 VALUES ($1, $2, $3, $4, $5) RETURNING id",
            )
            .bind(clean(&s.name))
            .bind(Json(&s.query))
            .bind(s.notify)
            .bind(s.last_checked)
            .bind(s.last_seen_created_at)
            .fetch_one(&self.pool)
            .await
            .map_err(db);
        }
        let updated = sqlx::query(
            "UPDATE saved_searches SET name = $2, query = $3, notify = $4, last_checked = $5,
                last_seen_created_at = $6 WHERE id = $1",
        )
        .bind(s.id)
        .bind(clean(&s.name))
        .bind(Json(&s.query))
        .bind(s.notify)
        .bind(s.last_checked)
        .bind(s.last_seen_created_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() == 0 {
            return Err(Error::Invalid(format!("no saved search with id {}", s.id)));
        }
        Ok(s.id)
    }

    async fn record_saved_search_run(
        &self,
        s: &SavedSearch,
        previous_seen: Option<i64>,
    ) -> Result<()> {
        // Watermark columns only (D77): a client's PUT during the run keeps its name, query
        // and notify, and a watermark it reset (changed query) is not set back.
        sqlx::query(
            "UPDATE saved_searches SET last_checked = $2,
                last_seen_created_at = CASE WHEN last_seen_created_at IS NOT DISTINCT FROM $3
                    THEN $4 ELSE last_seen_created_at END
             WHERE id = $1",
        )
        .bind(s.id)
        .bind(s.last_checked)
        .bind(previous_seen)
        .bind(s.last_seen_created_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn delete_saved_search(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM saved_searches WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn stats(&self) -> Result<StoreStats> {
        let r: StatsRow = sqlx::query_as(
            // Distinct infohashes / publishers come from trigger-kept counters (D43).
            "SELECT coalesce((SELECT value FROM meta WHERE key = 'infohash_count'), 0) AS torrents,
                    (SELECT count(*) FROM events) AS events,
                    (SELECT count(*) FROM publisher_counts) AS publishers,
                    (SELECT count(*) FROM comments) AS comments,
                    (SELECT count(*) FROM relays) AS relays,
                    pg_database_size(current_database()) AS db_bytes",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        let u = |v: i64| u64::try_from(v).unwrap_or(0);
        Ok(StoreStats {
            torrents: u(r.torrents),
            events: u(r.events),
            publishers: u(r.publishers),
            comments: u(r.comments),
            relays: u(r.relays),
            unsynced: 0,
            db_bytes: u(r.db_bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaceable_order() {
        assert!(is_newer(2, "b", 1, "a"));
        assert!(!is_newer(1, "a", 2, "b"));
        assert!(is_newer(5, "aa", 5, "ab"));
        assert!(!is_newer(5, "ab", 5, "aa"));
        assert!(!is_newer(5, "aa", 5, "aa"));
    }

    #[test]
    fn backoff() {
        assert_eq!(backoff_secs(1), 300);
        assert_eq!(backoff_secs(2), 600);
        assert_eq!(backoff_secs(9), 76_800);
        assert_eq!(backoff_secs(10), BACKOFF_MAX_SECS);
        assert_eq!(backoff_secs(500), BACKOFF_MAX_SECS);
    }

    #[test]
    fn trust_math() {
        assert_eq!(event_trust(0, false, 21), 20);
        assert_eq!(event_trust(90, false, 24), 100);
        assert_eq!(event_trust(0, true, 24), 0);
        assert_eq!(event_trust(100, true, 0), 100);
    }

    #[test]
    fn settings_parsing() {
        assert_eq!(json_string("\"abc\""), Some("abc".into()));
        assert_eq!(json_string("null"), None);
        assert_eq!(json_string("abc"), Some("abc".into()));
        assert_eq!(json_u8("20"), Some(20));
        assert_eq!(json_u8("\"35\""), Some(35));
        assert_eq!(json_u8("x"), None);
    }
}
