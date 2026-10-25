//! Explainable trust score (D18, wot-relays.md §9). Owner: trust-sync agent.

use crate::model::{OverrideAction, PublisherStats, TrustScore};
use crate::store::Store;

/// Everything needed to score one publisher (pure computation input).
#[derive(Clone, Debug, Default)]
pub struct TrustInputs {
    pub pubkey: String,
    pub root: Option<String>,
    pub override_action: Option<OverrideAction>,
    pub root_muted: bool,
    pub in_d1: bool,
    /// D1 members following this pubkey (hex), for FOLLOW_D2.
    pub d1_followers: Vec<String>,
    pub d1_muters: Vec<String>,
    /// (reporter, type) from root/D1 with type spam|malware|illegal.
    pub wot_reports: Vec<(String, String)>,
    pub wot_commenters: Vec<String>,
    pub stats: PublisherStats,
    pub relay_list_oversized: bool,
    pub now: i64,
    pub graph_version: i64,
}

/// Pure scoring (no PoW: that is per event, see `pow_bonus`). Factors sorted by |delta| desc.
pub fn compute(inputs: &TrustInputs) -> TrustScore {
    let _ = inputs;
    todo!("trust::compute")
}

/// 25 * clamp((bits - 8) / 16, 0, 1), rounded down.
pub fn pow_bonus(effective_bits: u8) -> u8 {
    let b = effective_bits as i32;
    ((25 * (b - 8).clamp(0, 16)) / 16) as u8
}

/// Per-event trust = clamp(publisher score + pow bonus, 0, 100); overrides/root-mute short-circuit
/// (score 0 or 100 ignores PoW).
pub fn event_trust(publisher: &TrustScore, effective_bits: u8) -> u8 {
    let _ = (publisher, effective_bits);
    todo!("trust::event_trust")
}

/// Gather inputs from the store and score one publisher (stores the result).
pub async fn score_publisher<S: Store + ?Sized>(
    store: &S,
    pubkey: &str,
    now: i64,
) -> crate::Result<TrustScore> {
    let _ = (store, pubkey, now);
    todo!("trust::score_publisher")
}

/// Rescore all publishers whose cached score is older than `max_age_secs` or from an older graph version.
pub async fn recompute<S: Store + ?Sized>(
    store: &S,
    now: i64,
    max_age_secs: i64,
) -> crate::Result<usize> {
    let _ = (store, now, max_age_secs);
    todo!("trust::recompute")
}
