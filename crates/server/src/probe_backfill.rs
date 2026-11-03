//! Header-probe backfill (D118, docs/PROBES.md §10): probes for torrents indexed before probes
//! existed, DHT rows and Nostr-only rows alike.
//!
//! The server keeps one queue row per infohash still to probe (`probe_backfill`, migration
//! `20261106000000`), filled in the background by [`fill_pass`] from `torrent_infohashes`.
//! A crawl worker with `MAGNETAR_PROBE_BACKFILL=true` leases a batch ([`lease`],
//! `probe/lease`), looks each hash up in the DHT, fetches the info dict again from a peer (the
//! fetcher verifies it against the infohash, so it cannot be made up), probes through the same
//! connection, and uploads the dict with its probe on the ordinary `dht/results` route. That
//! route counts a hash leased here to the uploading worker as held (D113's hold rule, [`held`]);
//! the dict then goes through the server crawler's own writer (classify, screen, store or
//! `not_new`), the probe through [`crate::probes::store`], and [`record`] closes the lease:
//! a stored probe deletes the queue row, anything else is retried later with backoff or
//! closed for good. Nothing a worker says changes a stored dict: an existing `dht_torrents`
//! row is never rewritten (`ON CONFLICT DO NOTHING`), a Nostr-only infohash gets its DHT row.
//!
//! Order (value first): torrents with the most known seeders, in power-of-two tiers, then the
//! newest (`born`); those with no seed check yet after every tier with seeders, those known
//! dead last. The order is fixed when a row is queued.

use std::collections::HashSet;

use magnetar_core::api::ProbeBackfillStats;
use magnetar_core::probe::PROBE_VERSION;
use sqlx::PgPool;

/// Most hashes per lease request.
pub const LEASE_MAX: usize = 256;
/// Infohashes read per fill step (keyset on `torrent_infohashes`' primary key).
pub const FILL_CHUNK: i64 = 5_000;
/// A fill pass starts again (to queue torrents indexed since) at most this often, and only
/// when a worker asks for a lease.
pub const FILL_EVERY_SECS: i64 = 6 * 3600;
/// Wait before the next try after an unsuccessful attempt (no peers, failed fetch, no probe):
/// 6 h, 3 days, 14 days; the row is closed after [`MAX_ATTEMPTS`].
pub const BACKOFF_SECS: [i64; 3] = [6 * 3600, 3 * 86400, 14 * 86400];
pub const MAX_ATTEMPTS: i16 = 4;
/// Queue rank (`rank` and first `due_at`) of a torrent whose stored probe is older than
/// [`PROBE_VERSION`] (D127): positive, so after every never-tried row (negative) and before
/// the retries (unix times).
pub const REPROBE_RANK: i64 = 1;
/// [`PROBE_VERSION`] as the `probe_attrs.probe_version` column type.
const PROBE_VERSION_I16: i16 = PROBE_VERSION as i16;
/// One seeders tier in the rank: `born` (a unix time) stays below it.
pub const TIER: i64 = 10_000_000_000;
/// Hourly counter (stats_series) of the `dht_torrents` rows a backfill dict added for a
/// Nostr-only torrent; counted here, not in `dht_torrents_added` (migration 20261106000000).
pub const DHT_ROWS_SERIES: &str = "probe_backfill_dht_rows";
/// Outcome texts are cut to this many bytes.
const OUTCOME_MAX: usize = 32;

/// The never-tried rank of a queue row (`due_at`, negative: lower = sooner): the seeders tier
/// (2 + ⌊log2 n⌋ with n known seeders or DHT peers, capped at 32; 1 = never checked; 0 = known
/// dead) times [`TIER`], plus `born` (newest first within a tier), negated, minus 1 (always
/// below 0, so before every retry and lease end).
pub fn rank(peers: Option<i64>, born: i64) -> i64 {
    let tier = match peers {
        None => 1,
        Some(n) if n <= 0 => 0,
        Some(n) => 2 + i64::from(63 - n.leading_zeros() as u16).min(30),
    };
    -(tier * TIER + born.clamp(0, TIER - 1)) - 1
}

/// SQL for [`rank`], over `liveness l` and `liveness_schedule s` (both LEFT JOINed).
const RANK_SQL: &str = "-(
    (CASE WHEN greatest(l.seeders, l.dht_peers_est) IS NULL THEN 1
          WHEN greatest(l.seeders, l.dht_peers_est) <= 0 THEN 0
          ELSE 2 + least(30, floor(log(2.0, greatest(l.seeders, l.dht_peers_est)::numeric)
                                   + 0.000000001)::int)
     END)::bigint * 10000000000
    + least(greatest(coalesce(s.born, 0), 0), 9999999999)) - 1";

/// Queue the next [`FILL_CHUNK`] infohashes after `after` (`""` to start) that have no probe
/// (or one older than [`PROBE_VERSION`], ranked [`REPROBE_RANK`], D127),
/// are not blocked and that a default search shows (a DHT row, or a visible Nostr event:
/// [`moderated`]). Adult torrents are queued too (D119). Returns the last infohash read
/// (None at the end of the table) and how many rows were queued. Rows already queued (or
/// closed) are kept as they are.
pub async fn fill_chunk(pool: &PgPool, after: &str) -> sqlx::Result<(Option<String>, u64)> {
    let sql = format!(
        "WITH chunk AS MATERIALIZED (
             SELECT infohash FROM torrent_infohashes WHERE infohash > $1
             ORDER BY infohash LIMIT $2
         ), ins AS (
             INSERT INTO probe_backfill (infohash, rank, due_at)
             SELECT c.infohash, r.rank, r.rank
             FROM chunk c
             LEFT JOIN liveness l ON l.infohash = c.infohash
             LEFT JOIN liveness_schedule s ON s.infohash = c.infohash
             LEFT JOIN probe_attrs p ON p.infohash = c.infohash
             CROSS JOIN LATERAL (SELECT CASE WHEN p.infohash IS NULL THEN {RANK_SQL}
                                             ELSE {REPROBE_RANK} END AS rank) r
             WHERE (p.infohash IS NULL OR p.probe_version < {PROBE_VERSION})
               AND NOT EXISTS (SELECT 1 FROM blocked_infohashes b WHERE b.infohash = c.infohash)
               AND {visible}
             ORDER BY c.infohash
             ON CONFLICT (infohash) DO NOTHING
             RETURNING 1
         )
         SELECT (SELECT max(infohash) FROM chunk), (SELECT count(*) FROM ins)",
        visible = visible_sql("c.infohash", "$3")
    );
    let min = min_trust(pool).await?;
    let (last, n): (Option<String>, i64) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(after)
        .bind(FILL_CHUNK)
        .bind(min)
        .fetch_one(pool)
        .await?;
    Ok((last, u64::try_from(n).unwrap_or(0)))
}

/// One walk over every indexed infohash ([`fill_chunk`] until the end). Returns rows queued.
pub async fn fill_pass(pool: &PgPool) -> sqlx::Result<u64> {
    let mut after = String::new();
    let mut total = 0;
    loop {
        let (last, n) = fill_chunk(pool, &after).await?;
        total += n;
        match last {
            Some(l) => after = l,
            None => return Ok(total),
        }
    }
}

/// Why a queue row was closed, or what an attempt ended with (`last_outcome`).
pub mod outcome {
    pub const BLOCKED: &str = "blocked";
    pub const GONE: &str = "gone";
    pub const NO_PROBE: &str = "no_probe";
}

/// What a lease handed out and what it closed on the way.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Leased {
    pub items: Vec<String>,
    /// Rows found already probed (deleted) or closed (blocked, gone).
    pub probed: u32,
    pub closed: u32,
    /// Rows left out of the queue because no default search shows them ([`moderated`]).
    pub moderated: u32,
}

/// Lease up to `max` due rows to `worker` until `now + ttl`. Before a row is handed out it is
/// checked again against what the server holds now: a probe stored meanwhile deletes it; a
/// blocked hash or an infohash with no row left closes it (adult rows are leased like any
/// other since D119); one no default search shows ([`moderated`]) leaves the queue (a later fill pass queues it again
/// once it is shown). Rows are taken with `FOR UPDATE SKIP LOCKED`, so concurrent leases never
/// share a hash. A batch that those checks emptied is followed by the next due rows (a few
/// rounds), so a run of closed rows does not send the worker idle.
pub async fn lease(
    pool: &PgPool,
    worker: &str,
    now: i64,
    max: usize,
    ttl: i64,
) -> sqlx::Result<Leased> {
    const ROUNDS: usize = 8;
    let max = max.clamp(1, LEASE_MAX);
    let mut tx = pool.begin().await?;
    let min = min_trust(&mut *tx).await?;
    let mut out = Leased::default();
    for _ in 0..ROUNDS {
        let want = max - out.items.len();
        let got = lease_round(&mut tx, worker, now, want, ttl, min, &mut out).await?;
        if got < want || out.items.len() >= max {
            break;
        }
    }
    tx.commit().await?;
    Ok(out)
}

/// One selection of up to `want` due rows; returns how many rows were selected.
async fn lease_round(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker: &str,
    now: i64,
    want: usize,
    ttl: i64,
    min: i16,
    out: &mut Leased,
) -> sqlx::Result<usize> {
    // The due index only (EXISTS in this select list was planned as a hashed scan of the
    // whole probe and block tables per lease); the checks below are primary-key lookups.
    let hashes: Vec<String> = sqlx::query_scalar(
        "SELECT infohash FROM probe_backfill
         WHERE due_at <= $1
         ORDER BY due_at
         LIMIT $2
         FOR UPDATE SKIP LOCKED",
    )
    .bind(now)
    .bind(want as i64)
    .fetch_all(&mut **tx)
    .await?;
    let selected = hashes.len();
    if hashes.is_empty() {
        return Ok(0);
    }
    let has_probe: HashSet<String> = sqlx::query_scalar(
        "SELECT infohash FROM probe_attrs WHERE infohash = ANY($1) AND probe_version >= $2",
    )
    .bind(&hashes)
    .bind(PROBE_VERSION_I16)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let blocked: HashSet<String> =
        sqlx::query_scalar("SELECT infohash FROM blocked_infohashes WHERE infohash = ANY($1)")
            .bind(&hashes)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
    // Still present: a DHT row or a Nostr row.
    let present: HashSet<String> = sqlx::query_scalar(
        "SELECT infohash FROM dht_torrents WHERE infohash = ANY($1)
         UNION SELECT infohash FROM torrents WHERE infohash = ANY($1)",
    )
    .bind(&hashes)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let hidden = moderated(&mut **tx, &hashes, min).await?;
    let mut drop = Vec::new();
    let mut close_h = Vec::new();
    let mut close_o: Vec<&str> = Vec::new();
    let mut items = Vec::new();
    for h in hashes {
        if has_probe.contains(&h) {
            out.probed += 1;
            drop.push(h);
            continue;
        }
        let why = if blocked.contains(&h) {
            Some(outcome::BLOCKED)
        } else if !present.contains(&h) {
            Some(outcome::GONE)
        } else {
            None
        };
        match why {
            Some(o) => {
                close_h.push(h);
                close_o.push(o);
            }
            None if hidden.contains(&h) => {
                out.moderated += 1;
                drop.push(h);
            }
            None => items.push(h),
        }
    }
    if !drop.is_empty() {
        sqlx::query("DELETE FROM probe_backfill WHERE infohash = ANY($1)")
            .bind(&drop)
            .execute(&mut **tx)
            .await?;
    }
    if !close_h.is_empty() {
        sqlx::query(
            "UPDATE probe_backfill b SET due_at = NULL, worker = NULL, expires_at = NULL,
                 last_outcome = u.o
             FROM unnest($1::text[], $2::text[]) AS u(h, o) WHERE b.infohash = u.h",
        )
        .bind(&close_h)
        .bind(&close_o)
        .execute(&mut **tx)
        .await?;
        out.closed += close_h.len() as u32;
    }
    if !items.is_empty() {
        let until = now + ttl.max(1);
        sqlx::query(
            "UPDATE probe_backfill SET worker = $2, leased_at = $3, expires_at = $4, due_at = $4
             WHERE infohash = ANY($1)",
        )
        .bind(&items)
        .bind(worker)
        .bind(now)
        .bind(until)
        .execute(&mut **tx)
        .await?;
        out.items.extend(items);
    }
    Ok(selected)
}

/// The trust threshold a default search applies (setting `trust.threshold`, default 20).
pub async fn min_trust(db: impl sqlx::PgExecutor<'_>) -> sqlx::Result<i16> {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(magnetar_core::store::keys::TRUST_THRESHOLD)
        .fetch_optional(db)
        .await?;
    Ok(i16::from(
        v.and_then(|v| crate::store::json_u8(&v))
            .unwrap_or(magnetar_core::store::DEFAULT_TRUST_THRESHOLD),
    ))
}

/// SQL: `h` (an infohash expression) has a row a default search shows as its own: a DHT row,
/// or a Nostr event from a publisher that is not blocked and reaches the trust threshold
/// (`min`, a bind) or is trusted (the rule of `search::push_hits_cte`, D43/D51).
fn visible_sql(h: &str, min: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM dht_torrents d WHERE d.infohash = {h})
          OR EXISTS (SELECT 1 FROM torrents t LEFT JOIN local_overrides o ON o.pubkey = t.pubkey
                     WHERE t.infohash = {h} AND o.action IS DISTINCT FROM 'block'
                       AND (t.trust >= {min} OR o.action IS NOT DISTINCT FROM 'trust')))"
    )
}

/// The infohashes among `hashes` that no default search shows: no DHT row, and every Nostr
/// event is from a blocked publisher (`local_overrides` block), or below the trust threshold
/// `min_trust` and not from a trusted one. A DHT row would bring them back as DHT torrents
/// (D51: a DHT row passes every threshold, and a block only hides rows with the publisher's
/// key), so backfill leaves them alone; they are queued again by a later fill pass once a
/// default search shows them (a block lifted, the threshold lowered).
pub async fn moderated(
    db: impl sqlx::PgExecutor<'_>,
    hashes: &[String],
    min_trust: i16,
) -> sqlx::Result<HashSet<String>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let sql = format!(
        "SELECT h FROM unnest($1::text[]) AS h WHERE NOT {}",
        visible_sql("h", "$2")
    );
    let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(hashes)
        .bind(min_trust)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Leased hashes `worker` gives back unworked (it is stopping): due again at once, in their
/// first place when never tried.
pub async fn release(
    pool: &PgPool,
    worker: &str,
    infohashes: &[String],
    now: i64,
) -> sqlx::Result<u64> {
    if infohashes.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "UPDATE probe_backfill SET worker = NULL, expires_at = NULL,
             due_at = CASE WHEN attempts = 0 THEN rank ELSE $3 END
         WHERE infohash = ANY($1) AND worker = $2",
    )
    .bind(infohashes)
    .bind(worker)
    .bind(now)
    .execute(pool)
    .await?
    .rows_affected())
}

/// The hashes among `hashes` leased here to `worker` and not answered yet: D113's hold rule
/// extended to backfill (an upload for them is applied as for a triaged hash).
pub async fn held(
    pool: &PgPool,
    worker: &str,
    hashes: &[[u8; 20]],
) -> sqlx::Result<HashSet<[u8; 20]>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let v: Vec<String> = hashes.iter().map(hex::encode).collect();
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT infohash FROM probe_backfill WHERE infohash = ANY($1) AND worker = $2",
    )
    .bind(&v)
    .bind(worker)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|h| crate::fleet::proto::parse_hash(h))
        .collect())
}

/// What a worker's answer for a leased hash means for its queue row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// A dict (with or without a probe). Deleted when a probe is stored for the infohash now;
    /// else an unsuccessful attempt ([`outcome::NO_PROBE`]).
    Dict,
    /// No dict, worth another try later (no peers, connect, timeout, ...): the failure code.
    Retry(String),
    /// No dict and never one worth probing (v2-only, invalid, oversize, blocked by
    /// the worker's blocklist): closed with this outcome.
    Close(String),
    /// A dict for a torrent no default search shows any more ([`moderated`], since the
    /// lease): not stored, and the row leaves the queue (a later fill pass queues it again
    /// once it is shown).
    Moderated,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    /// Rows deleted because a probe is stored.
    pub probed: u32,
    /// Rows waiting for a retry, and rows closed (by the answer, or after the last attempt).
    pub retried: u32,
    pub closed: u32,
}

fn cut(s: &str) -> String {
    let mut s: String = s.chars().filter(|c| !c.is_control()).collect();
    if s.len() > OUTCOME_MAX {
        let mut end = OUTCOME_MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Close the leases `worker` answered (after the dicts and probes of the same upload were
/// stored). Only rows still leased to `worker` move; each is answered once (`worker` cleared).
pub async fn record(
    pool: &PgPool,
    worker: &str,
    answers: &[(String, Answer)],
    now: i64,
) -> sqlx::Result<Recorded> {
    let mut r = Recorded::default();
    if answers.is_empty() {
        return Ok(r);
    }
    let mut tx = pool.begin().await?;
    let dicts: Vec<&str> = answers
        .iter()
        .filter(|(_, a)| *a == Answer::Dict)
        .map(|(h, _)| h.as_str())
        .collect();
    let probed: Vec<String> = sqlx::query_scalar(
        "DELETE FROM probe_backfill b WHERE b.infohash = ANY($1) AND b.worker = $2
             AND EXISTS (SELECT 1 FROM probe_attrs p WHERE p.infohash = b.infohash
                         AND p.probe_version >= $3)
         RETURNING b.infohash",
    )
    .bind(&dicts)
    .bind(worker)
    .bind(PROBE_VERSION_I16)
    .fetch_all(&mut *tx)
    .await?;
    r.probed = probed.len() as u32;
    let probed: HashSet<String> = probed.into_iter().collect();
    let (mut retry_h, mut retry_o, mut close_h, mut close_o) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut drop: Vec<&str> = Vec::new();
    for (h, a) in answers {
        match a {
            Answer::Moderated => drop.push(h.as_str()),
            Answer::Dict if probed.contains(h) => {}
            Answer::Dict => {
                retry_h.push(h.clone());
                retry_o.push(outcome::NO_PROBE.to_string());
            }
            Answer::Retry(code) => {
                retry_h.push(h.clone());
                retry_o.push(cut(code));
            }
            Answer::Close(code) => {
                close_h.push(h.clone());
                close_o.push(cut(code));
            }
        }
    }
    if !drop.is_empty() {
        r.closed +=
            sqlx::query("DELETE FROM probe_backfill WHERE infohash = ANY($1) AND worker = $2")
                .bind(&drop)
                .bind(worker)
                .execute(&mut *tx)
                .await?
                .rows_affected() as u32;
    }
    if !retry_h.is_empty() {
        let rows: Vec<Option<i64>> = sqlx::query_scalar(
            "UPDATE probe_backfill b SET attempts = b.attempts + 1, last_outcome = u.o,
                 worker = NULL, expires_at = NULL,
                 due_at = CASE WHEN b.attempts + 1 >= $4 THEN NULL
                               WHEN b.attempts + 1 = 1 THEN $5::bigint + $6
                               WHEN b.attempts + 1 = 2 THEN $5::bigint + $7
                               ELSE $5::bigint + $8 END
             FROM unnest($1::text[], $2::text[]) AS u(h, o)
             WHERE b.infohash = u.h AND b.worker = $3
             RETURNING b.due_at",
        )
        .bind(&retry_h)
        .bind(&retry_o)
        .bind(worker)
        .bind(MAX_ATTEMPTS)
        .bind(now)
        .bind(BACKOFF_SECS[0])
        .bind(BACKOFF_SECS[1])
        .bind(BACKOFF_SECS[2])
        .fetch_all(&mut *tx)
        .await?;
        for d in rows {
            if d.is_some() {
                r.retried += 1;
            } else {
                r.closed += 1;
            }
        }
    }
    if !close_h.is_empty() {
        r.closed += sqlx::query(
            "UPDATE probe_backfill b SET attempts = b.attempts + 1, last_outcome = u.o,
                 worker = NULL, expires_at = NULL, due_at = NULL
             FROM unnest($1::text[], $2::text[]) AS u(h, o)
             WHERE b.infohash = u.h AND b.worker = $3",
        )
        .bind(&close_h)
        .bind(&close_o)
        .bind(worker)
        .execute(&mut *tx)
        .await?
        .rows_affected() as u32;
    }
    tx.commit().await?;
    Ok(r)
}

/// A probe stored outside backfill (a new find's probe) ends that infohash's backfill too.
pub async fn forget_probed(pool: &PgPool, infohashes: &[String]) -> sqlx::Result<u64> {
    if infohashes.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "DELETE FROM probe_backfill b WHERE b.infohash = ANY($1)
             AND EXISTS (SELECT 1 FROM probe_attrs p WHERE p.infohash = b.infohash
                         AND p.probe_version >= $2)",
    )
    .bind(infohashes)
    .bind(PROBE_VERSION_I16)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Queue counts: one scan of the narrow queue table (~230 ms on 1.8M rows, so the API caches
/// it), plus the probe count.
pub async fn stats(pool: &PgPool, now: i64) -> sqlx::Result<ProbeBackfillStats> {
    let (waiting, leased, retrying): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE due_at < 0),
                count(*) FILTER (WHERE due_at >= 0 AND worker IS NOT NULL AND expires_at > $1),
                count(*) FILTER (WHERE due_at >= 0
                                 AND NOT (worker IS NOT NULL AND expires_at > $1))
         FROM probe_backfill WHERE due_at IS NOT NULL",
    )
    .bind(now)
    .fetch_one(pool)
    .await?;
    let closed: Vec<(Option<String>, i64)> = sqlx::query_as(
        "SELECT last_outcome, count(*) FROM probe_backfill WHERE due_at IS NULL
         GROUP BY 1 ORDER BY 2 DESC",
    )
    .fetch_all(pool)
    .await?;
    let probed: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_attrs")
        .fetch_one(pool)
        .await?;
    let u = |n: i64| u64::try_from(n).unwrap_or(0);
    Ok(ProbeBackfillStats {
        waiting: u(waiting),
        leased: u(leased),
        retrying: u(retrying),
        remaining: u(waiting + leased + retrying),
        closed: closed
            .into_iter()
            .map(|(o, n)| (o.unwrap_or_default(), u(n)))
            .collect(),
        probed: u(probed),
        filling: false,
        filled_at: None,
        at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order: more known seeders first (power-of-two tiers), never-checked after every
    /// tier with seeders, known dead last; newest first within a tier; all negative.
    #[test]
    fn rank_orders_by_seeders_then_newest() {
        let born = 1_790_000_000;
        assert!(rank(Some(100), born) < rank(Some(10), born));
        assert!(rank(Some(2), born) < rank(Some(1), born));
        assert_eq!(rank(Some(2), born), rank(Some(3), born), "same tier");
        assert!(rank(Some(1), born) < rank(None, born));
        assert!(rank(None, born) < rank(Some(0), born));
        assert!(
            rank(Some(10), born + 10) < rank(Some(10), born),
            "newest first"
        );
        assert!(rank(Some(1), 0) < rank(None, born + 100), "tier before age");
        assert!(rank(Some(i64::MAX), i64::MAX) < 0 && rank(Some(0), 0) < 0);
        assert!(
            rank(Some(0), born) < 0,
            "every never-tried row before any retry"
        );
    }

    #[test]
    fn outcomes_are_cut_and_clean() {
        assert_eq!(cut("no_peers"), "no_peers");
        assert_eq!(cut(&"é".repeat(40)).len(), 32);
        assert_eq!(cut("a\nb"), "ab");
    }
}
