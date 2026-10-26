//! Postgres side of the DHT pipeline (docs/DHT.md §3.7, D55): triage of sighted hashes against
//! `blocked_infohashes`, `torrent_infohashes` and `dht_seen`, the persisted queue (`dht_seen`
//! leases and retries), and the node identity / saved contacts in `settings`.

use std::net::{Ipv4Addr, SocketAddrV4};

use magnetar_core::dht::{CompactNode, Identity, Via};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// A queued row's lease: a fetch that never ran comes due again after this.
pub const LEASE_SECS: i64 = 3600;
/// An announce (a live peer) may retry a failed hash after this.
pub const ANNOUNCE_RETRY_SECS: i64 = 600;
/// Backoff after the 1st, 2nd, 3rd failed fetch; the 4th is final.
pub const BACKOFF_SECS: [i64; 3] = [3600, 6 * 3600, 24 * 3600];
pub const RETRY_BATCH: i64 = 1000;

pub const STATUS_QUEUED: i16 = 0;
pub const STATUS_FAILED: i16 = 2;
pub const STATUS_SKIPPED_KNOWN: i16 = 7;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TriageOutcome {
    pub input: usize,
    pub blocked: i64,
    pub known: i64,
    /// Hashes to fetch now (new, or due again).
    pub queued: Vec<[u8; 20]>,
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
    let hashes: Vec<Vec<u8>> = batch.iter().map(|(h, _)| h.to_vec()).collect();
    let vias: Vec<i16> = batch.iter().map(|(_, v)| v.code()).collect();
    let (blocked, known, queued): (i64, i64, Vec<Vec<u8>>) = sqlx::query_as(
        r#"
        WITH input AS (
            SELECT DISTINCT ON (h) h, via FROM unnest($1::bytea[], $2::int2[]) AS u(h, via)
            WHERE length(h) = 20
            ORDER BY h, (via = 1) DESC
        ), cls AS (
            SELECT i.h, i.via,
                   EXISTS (SELECT 1 FROM blocked_infohashes b
                           WHERE b.infohash = encode(i.h, 'hex') COLLATE "C") AS blocked,
                   (NOT $4 AND EXISTS (SELECT 1 FROM torrent_infohashes t
                           WHERE t.infohash = encode(i.h, 'hex') COLLATE "C")) AS known
            FROM input i
        ), up AS (
            INSERT INTO dht_seen AS s (infohash, first_seen, last_seen, via, status, attempts,
                                       next_attempt_at)
            SELECT h, $3, $3, via, 0, 0, $3 + $5 FROM cls WHERE NOT blocked AND NOT known
            ORDER BY h
            ON CONFLICT (infohash) DO UPDATE
                SET last_seen = EXCLUDED.last_seen, status = 0, via = EXCLUDED.via,
                    next_attempt_at = EXCLUDED.next_attempt_at
                WHERE (s.status IN (0, 2) AND s.next_attempt_at IS NOT NULL
                       AND s.next_attempt_at <= EXCLUDED.last_seen)
                   OR (s.status = 2 AND EXCLUDED.via = 1
                       AND s.last_seen <= EXCLUDED.last_seen - $6)
            RETURNING s.infohash
        )
        SELECT (SELECT count(*) FROM cls WHERE blocked),
               (SELECT count(*) FROM cls WHERE known AND NOT blocked),
               ARRAY(SELECT infohash FROM up)
        "#,
    )
    .bind(&hashes)
    .bind(&vias)
    .bind(now)
    .bind(fetch_known)
    .bind(LEASE_SECS)
    .bind(ANNOUNCE_RETRY_SECS)
    .fetch_one(pool)
    .await?;
    Ok(TriageOutcome {
        input: batch.len(),
        blocked,
        known,
        queued: queued
            .into_iter()
            .filter_map(|h| <[u8; 20]>::try_from(h.as_slice()).ok())
            .collect(),
    })
}

/// Rows due again (expired lease or failure backoff), at most `limit`: known/blocked ones are
/// settled (known → final `skipped_known`, blocked → deleted), the rest get a new lease and are
/// returned with their `via` code.
pub async fn take_due(pool: &PgPool, now: i64, limit: i64) -> sqlx::Result<Vec<([u8; 20], i16)>> {
    let rows: Vec<(Vec<u8>, i16)> = sqlx::query_as(
        r#"
        WITH due AS (
            SELECT infohash FROM dht_seen
            WHERE next_attempt_at IS NOT NULL AND next_attempt_at <= $1
            ORDER BY next_attempt_at LIMIT $2
            FOR UPDATE SKIP LOCKED
        ), cls AS (
            SELECT d.infohash,
                   EXISTS (SELECT 1 FROM blocked_infohashes b
                           WHERE b.infohash = encode(d.infohash, 'hex') COLLATE "C") AS blocked,
                   EXISTS (SELECT 1 FROM torrent_infohashes t
                           WHERE t.infohash = encode(d.infohash, 'hex') COLLATE "C") AS known
            FROM due d
        ), gone AS (
            DELETE FROM dht_seen s USING cls WHERE s.infohash = cls.infohash AND cls.blocked
        ), settled AS (
            UPDATE dht_seen s SET status = 7, next_attempt_at = NULL
            FROM cls WHERE s.infohash = cls.infohash AND cls.known AND NOT cls.blocked
        )
        UPDATE dht_seen s SET status = 0, last_seen = $1, next_attempt_at = $1 + $3
        FROM cls WHERE s.infohash = cls.infohash AND NOT cls.known AND NOT cls.blocked
        RETURNING s.infohash, s.via
        "#,
    )
    .bind(now)
    .bind(limit)
    .bind(LEASE_SECS)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(h, v)| Some((<[u8; 20]>::try_from(h.as_slice()).ok()?, v)))
        .collect())
}

/// A fetch failed (`error` is a short code: no_peers, connect, timeout, …): back off 1 h, 6 h,
/// 24 h, then never again (4 attempts).
pub async fn mark_failed(
    pool: &PgPool,
    info_hash: &[u8; 20],
    error: &str,
    now: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        UPDATE dht_seen SET status = 2, attempts = attempts + 1, last_error = left($2, 32),
            next_attempt_at = CASE attempts + 1 WHEN 1 THEN $3 + $4 WHEN 2 THEN $3 + $5
                                                WHEN 3 THEN $3 + $6 ELSE NULL END
        WHERE infohash = $1
        "#,
    )
    .bind(info_hash.as_slice())
    .bind(error)
    .bind(now)
    .bind(BACKOFF_SECS[0])
    .bind(BACKOFF_SECS[1])
    .bind(BACKOFF_SECS[2])
    .execute(pool)
    .await?;
    Ok(())
}

pub fn via_from_code(code: i16) -> Via {
    match code {
        1 => Via::Announce,
        2 => Via::GetPeers,
        _ => Via::Sample,
    }
}

#[derive(Serialize, Deserialize)]
struct IdentityJson {
    external_ip: String,
    ids: Vec<String>,
    ports: Vec<u16>,
}

pub const IDENTITY_KEY: &str = "dht.identity";
pub const NODES_KEY: &str = "dht.nodes";

async fn get_setting(pool: &PgPool, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

async fn put_setting(pool: &PgPool, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// The saved identity, only when it was made for the same ports (another port set means
/// another deployment of virtual nodes; the ids are derived again).
pub async fn load_identity(pool: &PgPool, ports: &[u16]) -> sqlx::Result<Option<Identity>> {
    let Some(raw) = get_setting(pool, IDENTITY_KEY).await? else {
        return Ok(None);
    };
    let Ok(j) = serde_json::from_str::<IdentityJson>(&raw) else {
        return Ok(None);
    };
    let Ok(ip) = j.external_ip.parse::<Ipv4Addr>() else {
        return Ok(None);
    };
    if j.ports != ports {
        return Ok(None);
    }
    let ids: Vec<[u8; 20]> = j
        .ids
        .iter()
        .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
        .collect();
    Ok(Some(Identity {
        external_ip: ip,
        ids,
    }))
}

pub async fn save_identity(pool: &PgPool, id: &Identity, ports: &[u16]) -> sqlx::Result<()> {
    let j = IdentityJson {
        external_ip: id.external_ip.to_string(),
        ids: id.ids.iter().map(hex::encode).collect(),
        ports: ports.to_vec(),
    };
    put_setting(
        pool,
        IDENTITY_KEY,
        &serde_json::to_string(&j).expect("json"),
    )
    .await
}

/// Saved contacts as `"<hex id>@<ip>:<port>"`.
pub async fn load_nodes(pool: &PgPool) -> sqlx::Result<Vec<CompactNode>> {
    let Some(raw) = get_setting(pool, NODES_KEY).await? else {
        return Ok(Vec::new());
    };
    let list: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|s| {
            let (id, addr) = s.split_once('@')?;
            Some(CompactNode {
                id: hex::decode(id).ok()?.try_into().ok()?,
                addr: addr.parse::<SocketAddrV4>().ok()?,
            })
        })
        .take(1000)
        .collect())
}

pub async fn save_nodes(pool: &PgPool, nodes: &[CompactNode]) -> sqlx::Result<()> {
    let list: Vec<String> = nodes
        .iter()
        .take(1000)
        .map(|n| format!("{}@{}", hex::encode(n.id), n.addr))
        .collect();
    put_setting(
        pool,
        NODES_KEY,
        &serde_json::to_string(&list).expect("json"),
    )
    .await
}
