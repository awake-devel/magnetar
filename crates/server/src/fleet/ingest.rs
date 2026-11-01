//! DHT side of the worker endpoints (D97): triage against the fleet-wide `dht_seen`, and
//! uploads of what workers fetched. Nothing a worker says about a torrent's content is
//! trusted: an uploaded info dict is hashed again against the infohash it was triaged under,
//! then classified and screened by the server's own classifier and blocklist (exactly
//! `dht::persist::persist_batch`, the server crawler's writer). The protocol has no field for
//! a category or a verdict on a dict. Nothing here logs a name.
//!
//! D113: every upload item (dict or status report) must be for a hash the uploading worker
//! holds: queued in `dht_seen` with `holder` = that worker (set by its own triage). Anything
//! else is ignored, so a worker cannot inject a dict it was never handed, nor finalise another
//! worker's or the server's hashes. A worker's block report blocks only the sighted hash (origin
//! `worker`); its claimed hybrid SHA-1 is dropped.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use magnetar_core::dht::Via;
use magnetar_core::metainfo::{FetchFailure, InfoError, Outcome, PeerError, outcome_from_raw};
use sqlx::PgPool;

use super::proto::*;
use super::{ApiResult, MAX_RESULTS, MAX_TRIAGE, WorkerName, json};
use crate::api::{ApiError, AppState};
use crate::crawlmap::{Layer, Metric};
use crate::dht::persist::{self, FetchResult};
use crate::dht::store;
use crate::geo::{GeoCode, GeoMode};

/// Index of a route in `RouteTriple` order: sample, get_peers, announce.
pub fn route_index(v: Via) -> usize {
    match v {
        Via::Sample => 0,
        Via::GetPeers => 1,
        Via::Announce => 2,
    }
}

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
    let mut batch: Vec<([u8; 20], Via)> = Vec::with_capacity(r.items.len());
    let mut geo: Vec<Option<GeoCode>> = Vec::with_capacity(r.items.len());
    let counting = st.crawlmap.mode() != GeoMode::Off;
    let mut by_route = [0u64; 3];
    for i in &r.items {
        let Some(h) = parse_hash(&i.h) else { continue };
        let via = via_of(&i.via);
        by_route[route_index(via)] += 1;
        batch.push((h, via));
        // D105: the worker resolved the node to codes; counted as `hashes` here.
        let code = (counting && (i.cc.is_some() || i.asn.is_some()))
            .then(|| GeoCode::from_parts(i.cc.as_deref(), i.asn));
        if let Some(c) = code {
            st.crawlmap.add(Layer::Dht, Metric::Hashes, c, 1);
        }
        geo.push(code);
    }
    st.crawlmap.add_upload(&r.geo_nodes, &r.as_names);
    let now = magnetar_core::now();
    let o = store::triage_for(
        st.store.pool(),
        &batch,
        &geo,
        now,
        st.fleet.config().fetch_known,
        Some(&name),
    )
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
        for (t, n) in e.triaged_by_route.iter_mut().zip(by_route) {
            *t += n;
        }
    });
    Ok(Json(TriageResponse {
        fetch: o.queued.iter().map(hex::encode).collect(),
        blocked: o.blocked as u32,
        known: o.known as u32,
    }))
}

/// A worker's block report: sighted hash, rule version.
type BlockReport = ([u8; 20], i16);

/// The `dht_seen` rows among `hashes` that `worker` holds (queued, `holder` = it, D113): the
/// only hashes it may upload a dict or report a status for.
pub async fn held(
    pool: &PgPool,
    worker: &str,
    hashes: &[[u8; 20]],
) -> sqlx::Result<HashSet<[u8; 20]>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let v: Vec<Vec<u8>> = hashes.iter().map(|h| h.to_vec()).collect();
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT infohash FROM dht_seen WHERE infohash = ANY($1) AND status = $2 AND holder = $3",
    )
    .bind(&v)
    .bind(store::STATUS_QUEUED)
    .bind(worker)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|h| <[u8; 20]>::try_from(h.as_slice()).ok())
        .collect())
}

/// Hashes a worker's blocklist stopped (hash only), each one the worker holds. Recorded under
/// origin `worker` (D113), so the hashes a worker's word blocked can be told apart (and
/// cleared) if a worker token leaks. The worker's claimed hybrid SHA-1 is not used: the server
/// never saw the dict, and the claim let a worker block any hash, even one queued by someone
/// else; a hybrid sighted under its v1 is fetched and screened on its own. The queued row goes.
async fn record_blocked(pool: &PgPool, items: &[BlockReport], now: i64) -> sqlx::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let hashes: Vec<String> = items.iter().map(|(s, _)| hex::encode(s)).collect();
    let versions: Vec<i16> = items.iter().map(|(_, v)| *v).collect();
    let sighted: Vec<Vec<u8>> = items.iter().map(|(s, _)| s.to_vec()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO blocked_infohashes (infohash, blocked_at, rule_version, origin)
         SELECT h, $2, v, 'worker' FROM unnest($1::text[], $3::int2[]) AS u(h, v) ORDER BY h
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
    let crawlmap = st.crawlmap.clone();
    let cfg = fleet.config();
    let now = magnetar_core::now();
    let mut ack = DhtAck::default();
    let worker_max = fleet.with(&name, |e| {
        e.hello
            .as_ref()
            .map_or(cfg.metadata_max as u64, |h| h.metadata_max)
    });

    // Only hashes this worker holds (D113), each once; the rest is ignored before any decoding.
    let sighted: Vec<[u8; 20]> = r
        .items
        .iter()
        .filter_map(|it| parse_hash(&it.sighted))
        .collect();
    let mine = held(pool, &name, &sighted).await.map_err(store_err)?;
    let mut seen: HashSet<[u8; 20]> = HashSet::new();

    // Decode first; dicts are verified together off the async threads.
    let mut dicts: Vec<([u8; 20], Via, usize, Vec<u8>)> = Vec::new();
    let mut reports: Vec<([u8; 20], Via, usize, DhtOutcome)> = Vec::new();
    for it in r.items {
        let Some(h) = parse_hash(&it.sighted) else {
            ack.rejected += 1;
            continue;
        };
        if !mine.contains(&h) || !seen.insert(h) {
            ack.ignored += 1;
            continue;
        }
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

    // Status reports: already limited to hashes this worker holds (above).
    let mut blocked: Vec<BlockReport> = Vec::new();
    for (h, via, peers, o) in reports {
        let result = match o {
            DhtOutcome::Metadata { .. } => unreachable!("split above"),
            // The claimed hybrid v1 is not used (see `record_blocked`).
            DhtOutcome::Blocked { rule_version, .. } => {
                blocked.push((h, rule_version));
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
        crawlmap.add_stored(&o.stored_geo);
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
