//! DHT side of the worker endpoints (D97): triage against the fleet-wide `dht_seen`, and
//! uploads of what workers fetched. Nothing a worker says about a torrent's content is
//! trusted: an uploaded info dict is hashed again against the infohash it was triaged under,
//! then classified and screened by the server's own classifier and blocklist (exactly
//! `dht::persist::persist_batch`, the server crawler's writer). The protocol has no field for
//! a category or a verdict on a dict. Nothing here logs a name.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use magnetar_core::dht::Via;
use magnetar_core::metainfo::{FetchFailure, InfoError, Outcome, PeerError, outcome_from_raw};
use sqlx::PgPool;

use super::proto::*;
use super::{ApiResult, MAX_RESULTS, MAX_TRIAGE, WorkerName, json};
use crate::api::{ApiError, AppState};
use crate::dht::persist::{self, FetchResult};
use crate::dht::store;

pub fn via_of(s: &str) -> Via {
    match s {
        "announce" => Via::Announce,
        "get_peers" => Via::GetPeers,
        _ => Via::Sample,
    }
}

pub async fn triage(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<TriageRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<TriageResponse>> {
    let r = json(body)?;
    if r.items.len() > MAX_TRIAGE {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!("at most {MAX_TRIAGE} hashes per triage"),
        ));
    }
    let batch: Vec<([u8; 20], Via)> = r
        .items
        .iter()
        .filter_map(|i| Some((parse_hash(&i.h)?, via_of(&i.via))))
        .collect();
    let now = magnetar_core::now();
    let o = store::triage(st.store.pool(), &batch, now, st.fleet.config().fetch_known)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store",
                persist::error_class(&e),
            )
        })?;
    st.fleet.with(&name, |e| {
        e.triaged += batch.len() as u64;
        e.queued += o.queued.len() as u64;
    });
    Ok(Json(TriageResponse {
        fetch: o.queued.iter().map(hex::encode).collect(),
        blocked: o.blocked as u32,
        known: o.known as u32,
    }))
}

/// A worker's block report: sighted hash, claimed SHA-1 of a hybrid, rule version.
type BlockReport = ([u8; 20], Option<[u8; 20]>, i16);

/// The `dht_seen` rows among `hashes` that are queued (status 0): what a worker may report a
/// status for.
async fn queued(pool: &PgPool, hashes: &[[u8; 20]]) -> sqlx::Result<HashSet<[u8; 20]>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let v: Vec<Vec<u8>> = hashes.iter().map(|h| h.to_vec()).collect();
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT infohash FROM dht_seen WHERE infohash = ANY($1) AND status = $2",
    )
    .bind(&v)
    .bind(store::STATUS_QUEUED)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|h| <[u8; 20]>::try_from(h.as_slice()).ok())
        .collect())
}

/// Hashes a worker's blocklist stopped (hash only). Only for queued hashes (the server handed
/// them out), and a hybrid's claimed SHA-1 only when nothing is stored under it, so a worker
/// cannot block what the index already holds. Same rows as the server's own block: the hashes
/// in `blocked_infohashes` (origin `dht`), the queued row gone.
async fn record_blocked(pool: &PgPool, items: &[BlockReport], now: i64) -> sqlx::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let extra: Vec<String> = items
        .iter()
        .filter_map(|(s, v1, _)| v1.filter(|v| v != s).map(hex::encode))
        .collect();
    let known: HashSet<String> = if extra.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar("SELECT infohash FROM torrent_infohashes WHERE infohash = ANY($1)")
            .bind(&extra)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect()
    };
    let mut hashes: Vec<String> = Vec::new();
    let mut versions: Vec<i16> = Vec::new();
    for (s, v1, rv) in items {
        hashes.push(hex::encode(s));
        versions.push(*rv);
        if let Some(v) = v1.filter(|v| v != s).map(hex::encode)
            && !known.contains(&v)
        {
            hashes.push(v);
            versions.push(*rv);
        }
    }
    let sighted: Vec<Vec<u8>> = items.iter().map(|(s, _, _)| s.to_vec()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO blocked_infohashes (infohash, blocked_at, rule_version, origin)
         SELECT h, $2, v, 'dht' FROM unnest($1::text[], $3::int2[]) AS u(h, v) ORDER BY h
         ON CONFLICT (infohash) DO NOTHING",
    )
    .bind(&hashes)
    .bind(now)
    .bind(&versions)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM dht_seen WHERE infohash = ANY($1)")
        .bind(&sighted)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

fn failure(code: &str) -> Option<FetchFailure> {
    let f = |error, peers_tried, skipped| FetchFailure {
        error,
        oversize: false,
        peers_tried,
        skipped,
    };
    Some(match code {
        "no_peers" => f(PeerError::Filtered, 0, 0),
        "fake_peers" => f(PeerError::Filtered, 0, 1),
        c => f(PeerError::from_code(c)?, 1, 0),
    })
}

/// One upload: verify, then write through the server crawler's own writer.
pub async fn dht_results(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<DhtResults>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<DhtAck>> {
    let r = json(body)?;
    if r.items.len() > MAX_RESULTS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!("at most {MAX_RESULTS} results per upload"),
        ));
    }
    let pool = st.store.pool();
    let fleet = st.fleet.clone();
    let cfg = fleet.config();
    let now = magnetar_core::now();
    let mut ack = DhtAck::default();
    let worker_max = fleet.with(&name, |e| {
        e.hello
            .as_ref()
            .map_or(cfg.metadata_max as u64, |h| h.metadata_max)
    });

    // Decode first; dicts are verified together off the async threads.
    let mut dicts: Vec<([u8; 20], Via, usize, Vec<u8>)> = Vec::new();
    let mut reports: Vec<([u8; 20], Via, usize, DhtOutcome)> = Vec::new();
    for it in r.items {
        let Some(h) = parse_hash(&it.sighted) else {
            ack.rejected += 1;
            continue;
        };
        let via = via_of(&it.via);
        let peers = it.peers_seen as usize;
        match it.outcome {
            DhtOutcome::Metadata { raw } => match data_encoding::BASE64.decode(raw.as_bytes()) {
                Ok(b) => dicts.push((h, via, peers, b)),
                Err(_) => ack.rejected += 1,
            },
            o => reports.push((h, via, peers, o)),
        }
    }

    let mut results: Vec<FetchResult> = Vec::new();
    if !dicts.is_empty() {
        let _permit = fleet.verify.acquire().await.map_err(|_| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "internal", "shutting down")
        })?;
        let max = cfg.metadata_max;
        let checked = tokio::task::spawn_blocking(move || {
            dicts
                .into_iter()
                .map(|(h, via, peers, raw)| (h, via, peers, outcome_from_raw(h, &raw, max)))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "verify"))?;
        for (h, via, peers, o) in checked {
            match o {
                Ok(outcome) => results.push(FetchResult {
                    sighted: h,
                    via,
                    peers_seen: peers,
                    result: Ok(outcome),
                }),
                Err(e) => {
                    ack.rejected += 1;
                    tracing::warn!(
                        worker = %name,
                        infohash = %hex::encode(h),
                        "worker upload refused: {e:?}"
                    );
                }
            }
        }
    }
    let verified = results
        .iter()
        .filter(|r| matches!(r.result, Ok(Outcome::Ok(_))))
        .count() as u64;

    // Status reports only for hashes the server handed out and still holds queued.
    let report_hashes: Vec<[u8; 20]> = reports.iter().map(|r| r.0).collect();
    let q = queued(pool, &report_hashes).await.map_err(store_err)?;
    let mut blocked: Vec<BlockReport> = Vec::new();
    let mut seen_reports: HashMap<[u8; 20], ()> = HashMap::new();
    for (h, via, peers, o) in reports {
        if !q.contains(&h) || seen_reports.insert(h, ()).is_some() {
            ack.ignored += 1;
            continue;
        }
        let result = match o {
            DhtOutcome::Metadata { .. } => unreachable!("split above"),
            DhtOutcome::Blocked {
                v1, rule_version, ..
            } => {
                let v1 = match v1.as_deref().map(parse_hash) {
                    Some(None) => {
                        ack.rejected += 1;
                        continue;
                    }
                    Some(Some(v)) => Some(v),
                    None => None,
                };
                blocked.push((h, v1, rule_version));
                continue;
            }
            DhtOutcome::Private => Ok(Outcome::Private),
            DhtOutcome::V2Only => Ok(Outcome::V2Only),
            DhtOutcome::Invalid { code } => match InfoError::from_code(&code) {
                Some(e) => Ok(Outcome::Invalid(e)),
                None => {
                    ack.rejected += 1;
                    continue;
                }
            },
            // Final only when the worker's limit is at least the server's: a smaller worker
            // leaves the torrent to someone with room (normal backoff, retried later).
            DhtOutcome::Oversize => Err(FetchFailure {
                error: PeerError::Oversize,
                oversize: worker_max >= cfg.metadata_max as u64,
                peers_tried: 1,
                skipped: 0,
            }),
            DhtOutcome::Failed { code } => match failure(&code) {
                Some(f) => Err(f),
                None => {
                    ack.rejected += 1;
                    continue;
                }
            },
        };
        ack.applied += 1;
        results.push(FetchResult {
            sighted: h,
            via,
            peers_seen: peers,
            result,
        });
    }

    record_blocked(pool, &blocked, now)
        .await
        .map_err(store_err)?;
    let worker_blocked = blocked.len() as u64;
    ack.blocked += worker_blocked as u32;
    let mut stored = 0u64;
    for chunk in results.chunks(persist::BATCH_MAX) {
        let o = persist::persist_batch(pool, chunk, fleet.screen.as_deref(), cfg.fetch_known, now)
            .await
            .map_err(store_err)?;
        stored += o.stored;
        ack.stored += o.stored as u32;
        ack.not_new += o.not_new as u32;
        ack.blocked += o.blocked as u32;
    }
    let rejected = u64::from(ack.rejected);
    fleet.with(&name, |e| {
        e.metadata += verified;
        e.metadata_rate.add(now, verified);
        e.stored += stored;
        e.blocked += worker_blocked;
        e.rejected += rejected;
    });
    Ok(Json(ack))
}

fn store_err(e: sqlx::Error) -> ApiError {
    // Only the class: a database message could quote a bound value (a name, D53).
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store",
        persist::error_class(&e),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_codes_round_trip() {
        for code in [
            "no_peers",
            "fake_peers",
            "connect",
            "timeout",
            "hash_mismatch",
        ] {
            assert_eq!(failure(code).unwrap().code(), code);
        }
        assert!(failure("nonsense").is_none());
    }
}
