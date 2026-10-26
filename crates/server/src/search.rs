//! Search SQL (ARCHITECTURE.md §5, rust-crates.md §9): eligible events are filtered first, then
//! grouped by infohash with one canonical event (highest trust, then earliest). Since D43 this is
//! the fallback while the Tantivy index (`search_index`) is disabled or still building.
//!
//! Rows come from the `torrent_rows` view (D50/D51): Nostr events and DHT rows. A DHT row
//! always passes `min_trust` (its `trust` is -1 so a Nostr event stays canonical), is not a
//! publisher, and is left out of the trigram typo arm (`dht_torrents` has no trigram index).

use magnetar_core::enrich::MIN_DISPLAY_CONFIDENCE;
use magnetar_core::enrich::ids::split_media_token;
use magnetar_core::model::{Liveness, Origin, TorrentSummary, npub};
use magnetar_core::query::{MIN_PREFIX_CHARS, SearchQuery, Sort, pg_tsquery};
use magnetar_core::release::ReleaseAttrs;
use sqlx::types::Json;
use sqlx::{FromRow, Postgres, QueryBuilder};

#[derive(FromRow)]
pub(crate) struct HitRow {
    event_id: String,
    infohash: String,
    pubkey: String,
    created_at: i64,
    trust: i16,
    title: String,
    category: String,
    total_size: Option<i64>,
    file_count: i32,
    pow_bits: i16,
    source_url: Option<String>,
    publisher_count: i64,
    rank: f32,
    seeders: Option<i32>,
    leechers: Option<i32>,
    best_peers: Option<i32>,
    expires_at: Option<i64>,
    media_title: Option<String>,
    media_year: Option<i32>,
    media_kind: Option<String>,
    media_id: Option<i64>,
    media_rating: Option<f32>,
    attrs: Option<Json<ReleaseAttrs>>,
    /// The canonical row's source; `store` refines it to `Both` from the page's infohashes.
    origin: String,
}

impl HitRow {
    pub(crate) fn into_summary(self, now: i64) -> TorrentSummary {
        let dead = match (self.expires_at, self.best_peers) {
            (Some(exp), Some(best)) if now < exp => Some(best == 0),
            _ => None,
        };
        TorrentSummary {
            npub: npub(&self.pubkey),
            infohash: self.infohash,
            event_id: self.event_id,
            title: self.title,
            category: self
                .category
                .parse()
                .unwrap_or(magnetar_core::model::Category::Other),
            total_size: self.total_size.and_then(|s| u64::try_from(s).ok()),
            file_count: u32::try_from(self.file_count).unwrap_or(0),
            created_at: self.created_at,
            pubkey: self.pubkey,
            seeders: self.seeders.and_then(|s| u32::try_from(s).ok()),
            leechers: self.leechers.and_then(|s| u32::try_from(s).ok()),
            dead,
            trust: self.trust.clamp(0, 100) as u8,
            pow_bits: self.pow_bits.clamp(0, 255) as u8,
            publisher_count: u32::try_from(self.publisher_count).unwrap_or(u32::MAX),
            source_url: self.source_url,
            rank: self.rank,
            pinned: false,
            hidden: false,
            media_title: self.media_title,
            media_year: self.media_year,
            media_kind: self.media_kind.and_then(|k| k.parse().ok()),
            matched_file: None,
            media_id: self.media_id,
            media_rating: self.media_rating,
            attrs: self.attrs.map(|a| a.0),
            origin: if self.origin == "dht" {
                Origin::Dht
            } else {
                Origin::Nostr
            },
            best_peers: self.best_peers.and_then(|b| u32::try_from(b).ok()),
        }
    }
}

/// Fresh liveness says nobody seeds; stale or unanswered is unknown (D17).
pub(crate) fn dead(l: &Liveness, now: i64) -> Option<bool> {
    match l.best_peers() {
        Some(best) if now < l.expires_at => Some(best == 0),
        _ => None,
    }
}

/// `WITH elig, grp, canon, hits` — everything up to the final SELECT.
fn push_hits_cte(qb: &mut QueryBuilder<Postgres>, q: &SearchQuery, min_trust: u8) {
    // `imdb:tt…` / `tmdb:movie:N` / `media:N` … select one title's torrents (D40).
    let (text, media) = split_media_token(&q.text);
    let text = text.trim().to_lowercase();
    let tsq = pg_tsquery(&text);
    qb.push("WITH elig AS (SELECT t.event_id, t.infohash, t.pubkey, t.created_at, t.trust, ");
    qb.push("t.title, t.category, t.total_size, t.file_count, t.pow_bits, t.source_url, ");
    match &tsq {
        Some(tsq) => {
            qb.push("(CASE WHEN t.search_tsv @@ to_tsquery('simple', ")
                .push_bind(tsq.clone())
                .push(") THEN 0.5 + 0.5 * ts_rank_cd(t.search_tsv, to_tsquery('simple', ")
                .push_bind(tsq.clone())
                .push("), 32) ELSE 0.5 * word_similarity(")
                .push_bind(text.clone())
                .push(", lower(t.title)) END)::float4 AS rank");
        }
        None => {
            qb.push("0::float4 AS rank");
        }
    }
    qb.push(", t.origin FROM torrent_rows t WHERE NOT EXISTS (SELECT 1 FROM local_overrides o ");
    // D51: a DHT row always passes the threshold (its trust is -1, below any threshold).
    qb.push(
        "WHERE o.pubkey = t.pubkey AND o.action = 'block') AND (t.origin = 'dht' OR t.trust >= ",
    )
    .push_bind(i16::from(min_trust))
    .push(" OR EXISTS (SELECT 1 FROM local_overrides o WHERE o.pubkey = t.pubkey ")
    .push("AND o.action = 'trust'))");
    if !q.categories.is_empty() {
        let cats: Vec<String> = q
            .categories
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        qb.push(" AND t.category = ANY(").push_bind(cats).push(")");
    }
    if let Some(min) = q.min_size {
        qb.push(" AND t.total_size >= ")
            .push_bind(i64::try_from(min).unwrap_or(i64::MAX));
    }
    if let Some(max) = q.max_size {
        qb.push(" AND t.total_size <= ")
            .push_bind(i64::try_from(max).unwrap_or(i64::MAX));
    }
    if let Some(since) = q.since {
        qb.push(" AND t.created_at >= ").push_bind(since);
    }
    if let Some(until) = q.until {
        qb.push(" AND t.created_at <= ").push_bind(until);
    }
    if let Some(m) = &media {
        crate::enrich::push_media_filter(qb, m);
    }
    push_attr_filters(qb, q);
    if let Some(tsq) = &tsq {
        qb.push(" AND (t.search_tsv @@ to_tsquery('simple', ")
            .push_bind(tsq.clone())
            .push(")");
        // Fuzzy title matching needs a few characters: below that pg_trgm cannot use its
        // index and `<%` scans every title.
        // Nostr rows only: `dht_torrents` has no trigram index, and the constant `origin` of
        // each UNION ALL branch folds this arm away in the DHT branch.
        if text.chars().count() >= MIN_PREFIX_CHARS {
            qb.push(" OR (t.origin = 'nostr' AND ")
                .push_bind(text.clone())
                .push(" <% lower(t.title))");
        }
        qb.push(")");
    }
    qb.push(
        "), grp AS (SELECT infohash, count(DISTINCT pubkey) FILTER (WHERE origin = 'nostr') \
         AS publisher_count, max(rank) AS rank FROM elig GROUP BY infohash), \
         canon AS (SELECT DISTINCT ON (infohash) * FROM elig \
         ORDER BY infohash, trust DESC, created_at ASC, event_id ASC), \
         hits AS (SELECT c.event_id, c.infohash, c.pubkey, c.created_at, c.trust, c.title, \
         c.category, c.total_size, c.file_count, c.pow_bits, c.source_url, g.publisher_count, \
         g.rank, c.origin, l.seeders, l.leechers, l.expires_at, \
         CASE WHEN l.seeders IS NULL AND l.dht_peers_est IS NULL THEN NULL \
         ELSE greatest(coalesce(l.seeders, 0), coalesce(l.dht_peers_est, 0)) END AS best_peers \
         FROM canon c JOIN grp g ON g.infohash = c.infohash \
         LEFT JOIN liveness l ON l.infohash = c.infohash) ",
    );
}

/// The release/media filters of D44 on event row `t` (same meaning as the index's collector).
fn push_attr_filters(qb: &mut QueryBuilder<Postgres>, q: &SearchQuery) {
    let filters = q.attr_filters();
    let (media, attrs): (Vec<_>, Vec<_>) = filters
        .into_iter()
        .partition(|(name, _)| matches!(*name, "genre" | "media_kind"));
    if !attrs.is_empty() {
        qb.push(" AND EXISTS (SELECT 1 FROM torrent_attrs a WHERE a.infohash = t.infohash");
        for (name, values) in attrs {
            // Column names come from this match, never from input.
            let (col, array) = match name {
                "language" => ("languages", true),
                "subtitles" => ("subtitles", true),
                "hdr" => ("hdr", true),
                "audio" => ("audio", true),
                "resolution" => ("resolution", false),
                "source" => ("source", false),
                _ => ("video_codec", false),
            };
            if array {
                qb.push(format!(" AND a.{col} && ")).push_bind(values);
            } else {
                qb.push(format!(" AND a.{col} = ANY("))
                    .push_bind(values)
                    .push(")");
            }
        }
        qb.push(")");
    }
    if !media.is_empty() || q.min_rating.is_some() {
        qb.push(
            " AND EXISTS (SELECT 1 FROM torrent_media tm JOIN media m ON m.id = tm.media_id \
             WHERE tm.infohash = t.infohash AND tm.confidence >= ",
        )
        .push_bind(MIN_DISPLAY_CONFIDENCE);
        for (name, values) in media {
            if name == "genre" {
                qb.push(" AND ARRAY(SELECT lower(g) FROM unnest(m.genres) g) && ")
                    .push_bind(values);
            } else {
                qb.push(" AND m.kind = ANY(").push_bind(values).push(")");
            }
        }
        if let Some(r) = q.min_rating {
            qb.push(" AND m.rating >= ").push_bind(r - 0.05);
        }
        qb.push(")");
    }
    if q.min_year.is_some() || q.max_year.is_some() {
        qb.push(
            " AND coalesce((SELECT m.year FROM torrent_media tm JOIN media m ON m.id = tm.media_id \
             WHERE tm.infohash = t.infohash AND tm.confidence >= ",
        )
        .push_bind(MIN_DISPLAY_CONFIDENCE)
        .push("), (SELECT a.year FROM torrent_attrs a WHERE a.infohash = t.infohash)) BETWEEN ")
        .push_bind(q.min_year.unwrap_or(i32::MIN))
        .push(" AND ")
        .push_bind(q.max_year.unwrap_or(i32::MAX));
    }
}

/// `WHERE ...` on `hits` (liveness filters).
fn push_hit_filters(qb: &mut QueryBuilder<Postgres>, q: &SearchQuery, now: i64) {
    qb.push(" WHERE TRUE");
    if q.hides_adult() {
        // D52: canonical category, or a DHT row the classifier tagged adult.
        qb.push(
            " AND h.category <> 'porn' AND NOT EXISTS (SELECT 1 FROM dht_torrents d \
             WHERE d.infohash = h.infohash AND d.category = 'porn')",
        );
    }
    // D51 source chip: a torrent known from both sources matches either value.
    match q.origin_filter().as_slice() {
        [o] if o == "dht" => {
            qb.push(" AND EXISTS (SELECT 1 FROM dht_torrents d WHERE d.infohash = h.infohash)");
        }
        [o] if o == "nostr" => {
            qb.push(
                " AND EXISTS (SELECT 1 FROM torrents t WHERE t.infohash = h.infohash \
                 AND NOT EXISTS (SELECT 1 FROM local_overrides o WHERE o.pubkey = t.pubkey \
                 AND o.action = 'block'))",
            );
        }
        _ => {}
    }
    if q.browse_hides_unchecked_dht() {
        // D76: the empty query lists a DHT-only torrent once a seed check found peers. The
        // canonical row is a DHT row only when no eligible Nostr event exists; one below
        // `min_trust` still makes the torrent "known from Nostr", as in the index.
        qb.push(
            " AND (h.origin <> 'dht' OR coalesce(h.best_peers, 0) > 0 OR EXISTS \
             (SELECT 1 FROM torrents t WHERE t.infohash = h.infohash))",
        );
    }
    if q.hide_dead {
        qb.push(" AND NOT coalesce(expires_at > ")
            .push_bind(now)
            .push(" AND best_peers = 0, false)");
    }
    if let Some(n) = q.min_seeders {
        qb.push(" AND coalesce(best_peers, 0) >= ")
            .push_bind(i32::try_from(n).unwrap_or(i32::MAX));
    }
}

/// The fallback count stops here: above it `total` is a lower bound ("1,000+", D43).
pub(crate) const COUNT_CAP: i64 = 1000;

/// Counts at most `COUNT_CAP + 1` hits.
pub(crate) fn count_query(q: &SearchQuery, now: i64, min_trust: u8) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::new("");
    push_hits_cte(&mut qb, q, min_trust);
    qb.push("SELECT count(*) FROM (SELECT 1 FROM hits h");
    push_hit_filters(&mut qb, q, now);
    qb.push(" LIMIT ").push_bind(COUNT_CAP + 1).push(") capped");
    qb
}

pub(crate) fn hits_query(q: &SearchQuery, now: i64, min_trust: u8) -> QueryBuilder<Postgres> {
    hits_query_with("", q, now, min_trust)
}

fn hits_query_with(
    prefix: &str,
    q: &SearchQuery,
    now: i64,
    min_trust: u8,
) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::new(prefix);
    push_hits_cte(&mut qb, q, min_trust);
    qb.push(
        "SELECT h.*, m.title AS media_title, m.year AS media_year, m.kind AS media_kind, \
         m.id AS media_id, m.rating AS media_rating, a.attrs \
         FROM hits h LEFT JOIN torrent_media tm ON tm.infohash = h.infohash AND tm.confidence >= ",
    )
    .push_bind(MIN_DISPLAY_CONFIDENCE)
    .push(
        " LEFT JOIN media m ON m.id = tm.media_id \
         LEFT JOIN torrent_attrs a ON a.infohash = h.infohash",
    );
    push_hit_filters(&mut qb, q, now);
    let dir = if q.descending { "DESC" } else { "ASC" };
    let order = match q.effective_sort() {
        // Relevance is always best-first; `descending` only orders the other sorts.
        Sort::Relevance => "h.rank DESC, h.created_at DESC".to_string(),
        Sort::Date => format!("h.created_at {dir}"),
        Sort::Size => format!("h.total_size {dir} NULLS LAST, h.created_at DESC"),
        Sort::Seeders => format!("h.best_peers {dir} NULLS LAST, h.created_at DESC"),
        Sort::Rating => format!("m.rating {dir} NULLS LAST, h.created_at DESC"),
    };
    qb.push(" ORDER BY ")
        .push(order)
        .push(", h.infohash ASC LIMIT ");
    qb.push_bind(i64::from(q.effective_limit()));
    qb.push(" OFFSET ").push_bind(i64::from(q.offset));
    qb
}

/// `EXPLAIN` of the fallback's page query (tests and ops checks: the DHT branch of
/// `torrent_rows` must reach `dht_torrents` through its GIN index, never a full scan, D51).
pub async fn explain_hits(
    conn: &mut sqlx::PgConnection,
    q: &SearchQuery,
    now: i64,
    min_trust: u8,
) -> magnetar_core::error::Result<Vec<String>> {
    let mut qb = hits_query_with("EXPLAIN ", q, now, min_trust);
    qb.build_query_scalar()
        .fetch_all(conn)
        .await
        .map_err(crate::store::db)
}
