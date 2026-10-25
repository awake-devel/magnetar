//! Search SQL (ARCHITECTURE.md §5, rust-crates.md §9): eligible events are filtered first, then
//! grouped by infohash with one canonical event (highest trust, then earliest).

use magnetar_core::model::{Liveness, TorrentSummary, npub};
use magnetar_core::query::{SearchQuery, Sort, pg_tsquery};
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
            category: self.category.parse().unwrap_or(magnetar_core::model::Category::Other),
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
    let text = q.text.trim().to_lowercase();
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
    qb.push(" FROM torrents t WHERE NOT EXISTS (SELECT 1 FROM local_overrides o ");
    qb.push("WHERE o.pubkey = t.pubkey AND o.action = 'block') AND (t.trust >= ")
        .push_bind(i16::from(min_trust))
        .push(" OR EXISTS (SELECT 1 FROM local_overrides o WHERE o.pubkey = t.pubkey ")
        .push("AND o.action = 'trust'))");
    if !q.categories.is_empty() {
        let cats: Vec<String> = q.categories.iter().map(|c| c.as_str().to_string()).collect();
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
    if let Some(tsq) = &tsq {
        qb.push(" AND (t.search_tsv @@ to_tsquery('simple', ")
            .push_bind(tsq.clone())
            .push(") OR ")
            .push_bind(text.clone())
            .push(" <% lower(t.title))");
    }
    qb.push(
        "), grp AS (SELECT infohash, count(DISTINCT pubkey) AS publisher_count, max(rank) AS rank \
         FROM elig GROUP BY infohash), \
         canon AS (SELECT DISTINCT ON (infohash) * FROM elig \
         ORDER BY infohash, trust DESC, created_at ASC, event_id ASC), \
         hits AS (SELECT c.event_id, c.infohash, c.pubkey, c.created_at, c.trust, c.title, \
         c.category, c.total_size, c.file_count, c.pow_bits, c.source_url, g.publisher_count, \
         g.rank, l.seeders, l.leechers, l.expires_at, \
         CASE WHEN l.seeders IS NULL AND l.dht_peers_est IS NULL THEN NULL \
         ELSE greatest(coalesce(l.seeders, 0), coalesce(l.dht_peers_est, 0)) END AS best_peers \
         FROM canon c JOIN grp g ON g.infohash = c.infohash \
         LEFT JOIN liveness l ON l.infohash = c.infohash) ",
    );
}

/// `WHERE ...` on `hits` (liveness filters).
fn push_hit_filters(qb: &mut QueryBuilder<Postgres>, q: &SearchQuery, now: i64) {
    qb.push(" WHERE TRUE");
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

pub(crate) fn count_query(q: &SearchQuery, now: i64, min_trust: u8) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::new("");
    push_hits_cte(&mut qb, q, min_trust);
    qb.push("SELECT count(*) FROM hits");
    push_hit_filters(&mut qb, q, now);
    qb
}

pub(crate) fn hits_query(q: &SearchQuery, now: i64, min_trust: u8) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::new("");
    push_hits_cte(&mut qb, q, min_trust);
    qb.push("SELECT * FROM hits");
    push_hit_filters(&mut qb, q, now);
    let dir = if q.descending { "DESC" } else { "ASC" };
    let order = match q.effective_sort() {
        // Relevance is always best-first; `descending` only orders the other sorts.
        Sort::Relevance => "rank DESC, created_at DESC".to_string(),
        Sort::Date => format!("created_at {dir}"),
        Sort::Size => format!("total_size {dir} NULLS LAST, created_at DESC"),
        Sort::Seeders => format!("best_peers {dir} NULLS LAST, created_at DESC"),
    };
    qb.push(" ORDER BY ").push(order).push(", infohash ASC LIMIT ");
    qb.push_bind(i64::from(q.effective_limit()));
    qb.push(" OFFSET ").push_bind(i64::from(q.offset));
    qb
}
