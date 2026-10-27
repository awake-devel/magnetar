//! A worker's share of the seed checks (D98): lease due infohashes from the server, check them
//! with the same `core::liveness::Scraper` as the server (its own tracker sockets, rate limits
//! and DHT node cache, from this machine's line), post the results. The server writes them as
//! its own. A stop mid-batch gives the lease back; a crash lets it expire.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use magnetar_core::liveness::{LivenessConfig, Scraper};
use serde::Serialize;
use tokio::sync::watch;

use super::client::{Client, ClientError};
use super::governor::Governor;
use crate::daemons::stop_requested;
use crate::fleet::proto::*;

/// Pause when nothing is due, and the longest pause after errors.
pub const IDLE: Duration = Duration::from_secs(60);
const ERROR_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct SeedCounters {
    pub leases: AtomicU64,
    pub leased: AtomicU64,
    pub checked: AtomicU64,
    pub accepted: AtomicU64,
    pub ignored: AtomicU64,
    pub released: AtomicU64,
    pub errors: AtomicU64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SeedReport {
    pub leases: u64,
    pub leased: u64,
    pub checked: u64,
    pub accepted: u64,
    pub ignored: u64,
    pub released: u64,
    pub errors: u64,
}

impl SeedCounters {
    pub fn report(&self) -> SeedReport {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        SeedReport {
            leases: g(&self.leases),
            leased: g(&self.leased),
            checked: g(&self.checked),
            accepted: g(&self.accepted),
            ignored: g(&self.ignored),
            released: g(&self.released),
            errors: g(&self.errors),
        }
    }
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

/// Send with retries on transient errors (≤ `tries`); fatal errors return at once.
pub async fn with_retry<T, F, Fut>(
    tries: u32,
    stop: &mut watch::Receiver<bool>,
    mut f: F,
) -> Result<T, ClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>>,
{
    let mut wait = Duration::from_secs(1);
    let mut n = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                n += 1;
                if n >= tries || *stop.borrow() {
                    return Err(e);
                }
                tracing::debug!("worker: {e}; retrying in {} s", wait.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_requested(stop) => return Err(e),
                }
                wait = (wait * 2).min(ERROR_MAX);
            }
        }
    }
}

/// Lease, check, post, until `stop`. Returns a fatal client error (stops the worker).
pub async fn run(
    client: Client,
    cfg: LivenessConfig,
    batch: u32,
    governor: Arc<Governor>,
    c: Arc<SeedCounters>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), ClientError> {
    let scraper = Scraper::new(cfg);
    let mut wait_err = Duration::from_secs(5);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        if governor.pressure() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(10)) => continue,
                _ = stop_requested(&mut stop) => return Ok(()),
            }
        }
        let lease: Lease = match client.post(LEASE, &LeaseRequest { max: batch }).await {
            Ok(l) => {
                wait_err = Duration::from_secs(5);
                l
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                add(&c.errors, 1);
                tracing::warn!("worker: seed-check lease: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(wait_err) => {}
                    _ = stop_requested(&mut stop) => return Ok(()),
                }
                wait_err = (wait_err * 2).min(ERROR_MAX);
                continue;
            }
        };
        if lease.items.is_empty() {
            tokio::select! {
                _ = tokio::time::sleep(IDLE) => continue,
                _ = stop_requested(&mut stop) => return Ok(()),
            }
        }
        add(&c.leases, 1);
        add(&c.leased, lease.items.len() as u64);
        let items: Vec<(String, Vec<String>)> = lease
            .items
            .into_iter()
            .map(|i| (i.infohash, i.trackers))
            .collect();
        let checked = tokio::select! {
            r = scraper.check(&items, magnetar_core::now()) => Some(r),
            _ = stop_requested(&mut stop) => None,
        };
        let Some((results, report)) = checked else {
            // Stopping: give the batch back so it does not wait for the lease to expire.
            let infohashes: Vec<String> = items.into_iter().map(|(h, _)| h).collect();
            let n = infohashes.len() as u64;
            if client
                .post::<_, Ack>(RELEASE, &Release { infohashes })
                .await
                .is_ok()
            {
                add(&c.released, n);
            }
            return Ok(());
        };
        add(&c.checked, results.len() as u64);
        tracing::info!(
            checked = report.hashes,
            alive = report.alive,
            dead = report.dead,
            unknown = report.unknown,
            "worker: seed-check batch in {} ms",
            report.ms
        );
        let body = LivenessResults { results };
        // Retries also run while stopping: the results are done, only the post is left.
        let (_tx, mut never) = watch::channel(false);
        match with_retry(5, &mut never, || {
            client.post::<_, Ack>(LIVENESS_RESULTS, &body)
        })
        .await
        {
            Ok(a) => {
                add(&c.accepted, u64::from(a.accepted));
                add(&c.ignored, u64::from(a.ignored));
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                add(&c.errors, 1);
                tracing::warn!("worker: posting seed checks failed (the lease will expire): {e}");
            }
        }
    }
}
