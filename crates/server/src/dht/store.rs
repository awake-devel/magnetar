//! Postgres side of DHT triage (docs/DHT.md §3.7, D55): sighted hashes against
//! `blocked_infohashes`, `torrent_infohashes` and `dht_seen`, and the persisted queue (`dht_seen`
//! leases and retries). The fleet ingest calls it for the crawl workers (D113, D156).

use magnetar_core::dht::Via;
use serde::Serialize;
use sqlx::PgPool;

/// A queued row's lease: a fetch that never ran comes due again after this.
pub const LEASE_SECS: i64 = 3600;
/// An announce (a live peer) may retry a failed hash after this.
pub const ANNOUNCE_RETRY_SECS: i64 = 600;
/// Backoff after the 1st, 2nd, 3rd failed fetch; the 4th is final.
pub const BACKOFF_SECS: [i64; 3] = [3600, 6 * 3600, 24 * 3600];

pub const STATUS_QUEUED: i16 = 0;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TriageOutcome {
    pub input: usize,
    pub blocked: i64,
    pub known: i64,
    /// Hashes to fetch now (new, or due again).
    pub queued: Vec<[u8; 20]>,
    /// Of `queued`, rows created by this triage (new to `dht_seen`).
    pub fresh: usize,
    /// New hashes left out by the `new_limit` of [`triage_for`].
    pub capped: usize,
}

/// One round trip for a batch: blocked → drop, known (Nostr or stored) → drop unless
/// `fetch_known`, in `dht_seen` and not due → drop (an announce may retry a `failed` row whose
/// last attempt is > 10 min old), otherwise insert/refresh as `queued` with a 1 h lease.
pub async fn triage(
    pool: &PgPool,
    batch: &[([u8; 20], Via)],
    now: i64,
    fetch_known: bool,
) -> sqlx::Result<TriageOutcome> {
    triage_geo(pool, batch, &[], now, fetch_known).await
}

/// [`triage`] with, per item, the country / network of the node that told us about it (D101;
/// parallel to `batch`, shorter = unresolved). A new row keeps it in `src_cc` / `src_asn` for
/// the crawl map's `stored` count; a re-queued row keeps its first one. Codes only, never an
/// address.
pub async fn triage_geo(
    pool: &PgPool,
    batch: &[([u8; 20], Via)],
    geo: &[Option<crate::geo::GeoCode>],
    now: i64,
    fetch_known: bool,
) -> sqlx::Result<TriageOutcome> {
    triage_for(pool, batch, geo, now, fetch_known, None, None).await
}

/// How long past its end a row a worker holds (its lease, or the backoff after a failure it
/// reported) must be before another worker's triage may take it over (D113).
pub const TAKEOVER_GRACE_SECS: i64 = LEASE_SECS;

/// [`triage_geo`] on behalf of `holder` (D113): the rows it queues (new, or due again) record
/// that crawl worker as their holder, so only it may upload a dict or a status for them. None =
/// no holder (the server's own crawler before D156; tests).
///
/// A due row held by another worker is re-queued for `holder` only once it is
/// [`TAKEOVER_GRACE_SECS`] past due (its holder is gone); NULL-holder rows, the holder's own
/// rows and a triage without a holder (`holder` None) are not held back. `new_limit` caps the
/// rows this call may create (the hourly per-worker cap, D113; None = no cap); re-queued rows
/// do not count.
pub async fn triage_for(
    pool: &PgPool,
    batch: &[([u8; 20], Via)],
    geo: &[Option<crate::geo::GeoCode>],
    now: i64,
    fetch_known: bool,
    holder: Option<&str>,
    new_limit: Option<i64>,
) -> sqlx::Result<TriageOutcome> {
    let hashes: Vec<Vec<u8>> = batch.iter().map(|(h, _)| h.to_vec()).collect();
    let vias: Vec<i16> = batch.iter().map(|(_, v)| v.code()).collect();
    let g = |i: usize| geo.get(i).copied().flatten();
    let ccs: Vec<Option<String>> = (0..batch.len())
        .map(|i| g(i).map(|c| c.cc_text()))
        .collect();
    let asns: Vec<Option<i32>> = (0..batch.len())
        .map(|i| g(i).map(|c| i32::try_from(c.asn).unwrap_or(0)))
        .collect();
    let (blocked, known, queued, fresh, candidates): (i64, i64, Vec<Vec<u8>>, i64, i64) =
        sqlx::query_as(
            r#"
        WITH input AS (
            SELECT DISTINCT ON (h) h, via, cc, asn
            FROM unnest($1::bytea[], $2::int2[], $7::text[], $8::int4[]) AS u(h, via, cc, asn)
            WHERE length(h) = 20
            ORDER BY h, (via = 1) DESC
        ), cls AS (
            SELECT i.h, i.via, i.cc, i.asn,
                   EXISTS (SELECT 1 FROM blocked_infohashes b
                           WHERE b.infohash = encode(i.h, 'hex') COLLATE "C") AS blocked,
                   (NOT $4 AND EXISTS (SELECT 1 FROM torrent_infohashes t
                           WHERE t.infohash = encode(i.h, 'hex') COLLATE "C")) AS known
            FROM input i
        ), cand AS (
            SELECT c.h, c.via, c.cc, c.asn,
                   EXISTS (SELECT 1 FROM dht_seen d WHERE d.infohash = c.h) AS present
            FROM cls c
            WHERE NOT c.blocked AND NOT c.known
        ), pick AS (
            SELECT h, via, cc, asn FROM cand WHERE present
            UNION ALL
            (SELECT h, via, cc, asn FROM cand WHERE NOT present ORDER BY h LIMIT $10)
        ), up AS (
            INSERT INTO dht_seen AS s (infohash, first_seen, last_seen, via, status, attempts,
                                       next_attempt_at, src_cc, src_asn, holder)
            SELECT h, $3, $3, via, 0, 0, $3 + $5, cc, asn, $9 FROM pick
            ORDER BY h
            ON CONFLICT (infohash) DO UPDATE
                SET last_seen = EXCLUDED.last_seen, status = 0, via = EXCLUDED.via,
                    next_attempt_at = EXCLUDED.next_attempt_at, holder = EXCLUDED.holder,
                    src_cc = COALESCE(s.src_cc, EXCLUDED.src_cc),
                    src_asn = CASE WHEN s.src_cc IS NULL THEN EXCLUDED.src_asn ELSE s.src_asn END
                WHERE ((s.status IN (0, 2) AND s.next_attempt_at IS NOT NULL
                        AND s.next_attempt_at <= EXCLUDED.last_seen)
                    OR (s.status = 2 AND EXCLUDED.via = 1
                        AND s.last_seen <= EXCLUDED.last_seen - $6))
                  -- D113: no takeover of another worker's row until it is long past due.
                  AND (EXCLUDED.holder IS NULL OR s.holder IS NULL
                       OR s.holder = EXCLUDED.holder
                       OR (s.next_attempt_at IS NOT NULL
                           AND s.next_attempt_at <= EXCLUDED.last_seen - $11))
            RETURNING s.infohash, (s.xmax = 0) AS fresh
        )
        SELECT (SELECT count(*) FROM cls WHERE blocked),
               (SELECT count(*) FROM cls WHERE known AND NOT blocked),
               ARRAY(SELECT infohash FROM up),
               (SELECT count(*) FROM up WHERE fresh),
               (SELECT count(*) FROM cand WHERE NOT present)
        "#,
        )
        .bind(&hashes)
        .bind(&vias)
        .bind(now)
        .bind(fetch_known)
        .bind(LEASE_SECS)
        .bind(ANNOUNCE_RETRY_SECS)
        .bind(&ccs)
        .bind(&asns)
        .bind(holder)
        .bind(new_limit.map(|n| n.max(0)))
        .bind(TAKEOVER_GRACE_SECS)
        .fetch_one(pool)
        .await?;
    let fresh = usize::try_from(fresh).unwrap_or(0);
    let capped = new_limit.map_or(0, |l| usize::try_from(candidates - l.max(0)).unwrap_or(0));
    Ok(TriageOutcome {
        input: batch.len(),
        blocked,
        known,
        queued: queued
            .into_iter()
            .filter_map(|h| <[u8; 20]>::try_from(h.as_slice()).ok())
            .collect(),
        fresh,
        capped,
    })
}
