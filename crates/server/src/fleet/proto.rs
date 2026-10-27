//! Wire format between the server and its crawl workers (D96–D98, docs/WORKERS.md). JSON over
//! the server's HTTPS API under `/api/v1/worker/`, with a worker token and the
//! [`API_HEADER`] on every request. Infohashes are 40 lowercase hex characters; an info dict
//! travels as the raw bencoded bytes in standard base64, so the server can hash exactly what
//! the swarm sent.
//!
//! Compatibility rule: any change a peer of the previous [`WORKER_API`] could misread bumps
//! the number; the server refuses a worker of another number (409 `worker_api_mismatch`) and
//! the worker stops with that message. New optional fields with a default do not need a bump.

use magnetar_core::api::WorkerDhtState;
use magnetar_core::model::Liveness;
use serde::{Deserialize, Serialize};

/// The worker API version this build speaks.
pub const WORKER_API: u32 = 1;
/// Request header carrying the worker's [`WORKER_API`].
pub const API_HEADER: &str = "x-magnetar-worker-api";

pub const HELLO: &str = "/api/v1/worker/hello";
pub const HEARTBEAT: &str = "/api/v1/worker/heartbeat";
pub const LEASE: &str = "/api/v1/worker/liveness/lease";
pub const LIVENESS_RESULTS: &str = "/api/v1/worker/liveness/results";
pub const RELEASE: &str = "/api/v1/worker/liveness/release";
pub const TRIAGE: &str = "/api/v1/worker/dht/triage";
pub const DHT_RESULTS: &str = "/api/v1/worker/dht/results";

/// Sent once at start (and again after the server restarted).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hello {
    /// `magnetar-server --version` of the worker.
    pub version: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    pub liveness: bool,
    /// DHT mode (`polite` / `aggressive`), None when the worker does not crawl.
    pub dht: Option<String>,
    pub dht_ports: Vec<u16>,
    /// The worker's largest info dict (`MAGNETAR_DHT_METADATA_MAX`): a torrent above it is
    /// left to a worker or server with a larger limit instead of being marked final.
    pub metadata_max: u64,
    /// Version of the worker's built-in blocklist (the server screens again anyway).
    pub blocklist_version: i16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HelloResponse {
    /// The name the worker's token was configured with.
    pub name: String,
    pub server_version: String,
    pub worker_api: u32,
    /// Seed-check lease length, seconds.
    pub lease_secs: u64,
    /// Most infohashes per lease, per triage request and per result upload.
    pub max_lease: u32,
    pub max_triage: u32,
    pub max_results: u32,
    /// Largest request body the worker endpoints accept, bytes.
    pub max_body: u64,
    /// The server's `MAGNETAR_DHT_METADATA_MAX`.
    pub metadata_max: u64,
}

/// Every ~30 s: what the server shows in `/api/v1/stats.workers`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Heartbeat {
    pub uptime_secs: u64,
    pub rss_mb: Option<f64>,
    pub mem_available_mb: Option<f64>,
    /// Slowed down for memory or connection-tracking pressure.
    pub pressure: bool,
    pub dht: Option<WorkerDhtState>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LeaseRequest {
    pub max: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LeaseItem {
    pub infohash: String,
    pub trackers: Vec<String>,
}

/// Due infohashes reserved for this worker until `expires_at` (unix time); never handed to
/// anyone else meanwhile. Unanswered at expiry, they are due again for everyone.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Lease {
    pub expires_at: i64,
    pub items: Vec<LeaseItem>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LivenessResults {
    pub results: Vec<Liveness>,
}

/// Leased infohashes the worker gives back unchecked (it is stopping): due again at once.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Release {
    pub infohashes: Vec<String>,
}

/// `accepted`: written as the server's own check; `ignored`: not leased to this worker (any
/// more) or malformed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ack {
    pub accepted: u32,
    pub ignored: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TriageItem {
    pub h: String,
    /// `sample`, `announce` or `get_peers`.
    pub via: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TriageRequest {
    pub items: Vec<TriageItem>,
}

/// The hashes this worker should fetch now: new to the whole fleet, or due again. Each is
/// leased to it in `dht_seen` (1 h) so nobody else fetches it meanwhile.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TriageResponse {
    pub fetch: Vec<String>,
    pub blocked: u32,
    pub known: u32,
}

/// What became of one hash the worker was told to fetch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DhtOutcome {
    /// Verified, parsed and passed the worker's blocklist: the raw info dict, base64. The
    /// server verifies it against `sighted` again and classifies and screens it itself.
    Metadata {
        raw: String,
    },
    /// The worker's blocklist stopped it: hash only, nothing named leaves the worker. `v1` is
    /// the SHA-1 of a hybrid sighted under its truncated v2 hash. `tier` is for counts only.
    Blocked {
        #[serde(default)]
        v1: Option<String>,
        #[serde(default)]
        tier: String,
        #[serde(default)]
        rule_version: i16,
    },
    /// Verified but not storable (BEP 27 private, pure v2, unusable dict: `code` as
    /// `InfoError::code`).
    Private,
    V2Only,
    Invalid {
        code: String,
    },
    /// Every peer offered more than the worker's `metadata_max`.
    Oversize,
    /// No verified dict: `code` as `FetchFailure::code` (`no_peers`, `fake_peers`, `connect`,
    /// `timeout`, …).
    Failed {
        code: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DhtResult {
    /// The hash it was triaged under.
    pub sighted: String,
    #[serde(default)]
    pub via: String,
    #[serde(default)]
    pub peers_seen: u32,
    #[serde(flatten)]
    pub outcome: DhtOutcome,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DhtResults {
    pub items: Vec<DhtResult>,
}

/// What the server did with an upload (counts per item).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DhtAck {
    /// Verified dicts that became new rows / that were already stored.
    pub stored: u32,
    pub not_new: u32,
    /// Blocked: by the server's own screen of an uploaded dict, or reported by the worker.
    pub blocked: u32,
    /// Refused: hash mismatch, too large, bad base64 or hash, unknown code.
    pub rejected: u32,
    /// Statuses written (private, v2-only, invalid, oversize, failures).
    pub applied: u32,
    /// Reports for hashes not queued (lease expired and handed out again, or never queued).
    pub ignored: u32,
}

pub fn parse_hash(h: &str) -> Option<[u8; 20]> {
    if h.len() != 40 {
        return None;
    }
    hex::decode(h).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_tagged() {
        let r = DhtResult {
            sighted: "ab".repeat(20),
            via: "sample".into(),
            peers_seen: 3,
            outcome: DhtOutcome::Blocked {
                v1: None,
                tier: "hard".into(),
                rule_version: 1,
            },
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["kind"], "blocked");
        assert!(j.get("raw").is_none(), "a blocked report carries no dict");
        assert_eq!(serde_json::from_value::<DhtResult>(j).unwrap(), r);
        let f: DhtResult =
            serde_json::from_str(r#"{"sighted":"x","kind":"failed","code":"no_peers","extra":1}"#)
                .unwrap();
        assert_eq!(
            f.outcome,
            DhtOutcome::Failed {
                code: "no_peers".into()
            }
        );
        assert!(serde_json::from_str::<DhtResult>(r#"{"sighted":"x","kind":"what"}"#).is_err());
    }

    #[test]
    fn hashes() {
        assert_eq!(parse_hash(&"0a".repeat(20)), Some([10; 20]));
        assert_eq!(parse_hash(&"0A".repeat(20)), Some([10; 20]));
        assert!(parse_hash("0a").is_none());
        assert!(parse_hash(&"zz".repeat(20)).is_none());
    }
}
