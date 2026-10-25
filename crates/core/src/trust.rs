//! Explainable trust score (D18, wot-relays.md §9). Owner: trust-sync agent.

use std::collections::{HashMap, HashSet};

use crate::model::{OverrideAction, PublisherStats, TrustFactor, TrustScore};
use crate::store::{Store, keys};

pub const ROOT: i32 = 100;
pub const FOLLOW_D1: i32 = 70;
pub const FOLLOW_D2_MAX: f64 = 50.0;
pub const PROFILE: i32 = 5;
pub const AGE: i32 = 5;
pub const AGE_SECS: i64 = 30 * 86_400;
pub const COMMENT_EACH: i32 = 3;
pub const COMMENT_CAP: i32 = 10;
pub const MUTED_EACH: i32 = -20;
pub const MUTED_CAP: i32 = -60;
pub const REPORTED_EACH: i32 = -15;
pub const REPORTED_CAP: i32 = -45;
pub const OVERSIZED_RELAYLIST: i32 = -15;
pub const BURST: i32 = -10;
pub const BURST_PER_HOUR: u32 = 200;
/// NIP-56 report types that count against a publisher.
pub const REPORT_TYPES: &[&str] = &["spam", "malware", "illegal"];

/// Factor codes whose score ignores everything else (and PoW).
const SHORT_CIRCUIT: &[&str] = &["BLOCKED", "TRUSTED", "ROOT_MUTED"];

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

fn factor(code: &str, delta: i32, detail: String, evidence: Vec<String>) -> TrustFactor {
    TrustFactor {
        code: code.to_string(),
        delta,
        detail,
        evidence,
    }
}

/// Sorted, deduped, without `exclude`.
fn distinct(v: &[String], exclude: &str) -> Vec<String> {
    let mut v: Vec<String> = v.iter().filter(|s| *s != exclude).cloned().collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// FOLLOW_D2 points for `n2` D1 followers: 50 * (1 - exp(-n2/4)), rounded.
pub fn follow_d2_points(n2: usize) -> i32 {
    if n2 == 0 {
        return 0;
    }
    (FOLLOW_D2_MAX * (1.0 - (-(n2 as f64) / 4.0).exp())).round() as i32
}

/// Pure scoring (no PoW: that is per event, see `pow_bonus`). Factors sorted by |delta| desc.
pub fn compute(inputs: &TrustInputs) -> TrustScore {
    let pk = inputs.pubkey.as_str();
    let done = |score: u8, factors: Vec<TrustFactor>| TrustScore {
        pubkey: inputs.pubkey.clone(),
        score,
        factors,
        root: inputs.root.clone(),
        computed_at: inputs.now,
        graph_version: inputs.graph_version,
    };

    // Overrides short-circuit: the factor is shown alone.
    let short = match inputs.override_action {
        Some(OverrideAction::Block) => Some((0, "BLOCKED", "blocked by you", vec![])),
        Some(OverrideAction::Trust) => Some((100, "TRUSTED", "trusted by you", vec![])),
        None if inputs.root_muted => Some((
            0,
            "ROOT_MUTED",
            "muted by you (your public mute list)",
            inputs.root.iter().cloned().collect(),
        )),
        None => None,
    };
    if let Some((score, code, detail, evidence)) = short {
        let delta = if score == 0 { -100 } else { 100 };
        return done(score, vec![factor(code, delta, detail.into(), evidence)]);
    }

    let is_root = inputs.root.as_deref() == Some(pk);
    let mut pos = Vec::new();
    if is_root {
        pos.push(factor(
            "ROOT",
            ROOT,
            "this is you (trust root)".into(),
            vec![],
        ));
    } else if inputs.in_d1 {
        pos.push(factor(
            "FOLLOW_D1",
            FOLLOW_D1,
            "followed by you".into(),
            inputs.root.iter().cloned().collect(),
        ));
    } else {
        let followers = distinct(&inputs.d1_followers, pk);
        let pts = follow_d2_points(followers.len());
        if pts > 0 {
            pos.push(factor(
                "FOLLOW_D2",
                pts,
                format!("followed by {} of your follows", followers.len()),
                followers,
            ));
        }
    }
    let st = &inputs.stats;
    if st.has_profile && st.has_relay_list && !inputs.relay_list_oversized {
        pos.push(factor(
            "PROFILE",
            PROFILE,
            "has a profile and a relay list".into(),
            vec![],
        ));
    }
    if let Some(first) = st.first_seen.filter(|t| *t < inputs.now - AGE_SECS) {
        let days = (inputs.now - first) / 86_400;
        pos.push(factor(
            "AGE",
            AGE,
            format!("first seen {days} days ago"),
            vec![],
        ));
    }
    let commenters = distinct(&inputs.wot_commenters, pk);
    if !commenters.is_empty() {
        let pts = (COMMENT_EACH * commenters.len() as i32).min(COMMENT_CAP);
        pos.push(factor(
            "COMMENTS",
            pts,
            format!(
                "{} in your network commented on their torrents",
                plural(commenters.len(), "person", "people")
            ),
            commenters,
        ));
    }

    // Positive sum is capped at 100: trim the smallest contributions first so deltas add up.
    let mut excess = pos.iter().map(|f| f.delta).sum::<i32>() - 100;
    pos.sort_by_key(|f| f.delta);
    for f in pos.iter_mut() {
        if excess <= 0 {
            break;
        }
        let cut = excess.min(f.delta);
        f.delta -= cut;
        excess -= cut;
    }
    pos.retain(|f| f.delta > 0);

    let mut neg = Vec::new();
    let muters = distinct(&inputs.d1_muters, pk);
    if !muters.is_empty() {
        neg.push(factor(
            "MUTED_D1",
            (MUTED_EACH * muters.len() as i32).max(MUTED_CAP),
            format!(
                "muted by {} you follow",
                plural(muters.len(), "person", "people")
            ),
            muters,
        ));
    }
    let mut by_reporter: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (r, ty) in &inputs.wot_reports {
        if r != pk && REPORT_TYPES.contains(&ty.as_str()) {
            by_reporter.entry(r).or_default().insert(ty);
        }
    }
    if !by_reporter.is_empty() {
        let mut reporters: Vec<String> = by_reporter.keys().map(|s| s.to_string()).collect();
        reporters.sort_unstable();
        let mut types: Vec<&str> = by_reporter.values().flatten().copied().collect();
        types.sort_unstable();
        types.dedup();
        neg.push(factor(
            "REPORTED",
            (REPORTED_EACH * reporters.len() as i32).max(REPORTED_CAP),
            format!(
                "reported ({}) by {} in your network",
                types.join(", "),
                plural(reporters.len(), "person", "people")
            ),
            reporters,
        ));
    }
    if inputs.relay_list_oversized {
        neg.push(factor(
            "OVERSIZED_RELAYLIST",
            OVERSIZED_RELAYLIST,
            format!(
                "relay list has more than {} relays (spam)",
                crate::wot::MAX_RELAY_LIST
            ),
            vec![],
        ));
    }
    if st.max_per_hour > BURST_PER_HOUR && !inputs.in_d1 && !st.is_bridge {
        neg.push(factor(
            "BURST",
            BURST,
            format!("published {} torrents within one hour", st.max_per_hour),
            vec![],
        ));
    }

    let total: i32 = pos.iter().chain(&neg).map(|f| f.delta).sum();
    let mut factors: Vec<TrustFactor> = pos.into_iter().chain(neg).collect();
    factors.sort_by_key(|f| std::cmp::Reverse(f.delta.abs()));
    done(total.clamp(0, 100) as u8, factors)
}

/// 25 * clamp((bits - 8) / 16, 0, 1), rounded down.
pub fn pow_bonus(effective_bits: u8) -> u8 {
    let b = effective_bits as i32;
    ((25 * (b - 8).clamp(0, 16)) / 16) as u8
}

/// True if the score came from an override / root mute (PoW must not change it).
pub fn is_short_circuit(s: &TrustScore) -> bool {
    s.factors.len() == 1 && SHORT_CIRCUIT.contains(&s.factors[0].code.as_str())
}

/// Per-event trust = clamp(publisher score + pow bonus, 0, 100); overrides/root-mute short-circuit
/// (score 0 or 100 ignores PoW).
pub fn event_trust(publisher: &TrustScore, effective_bits: u8) -> u8 {
    if is_short_circuit(publisher) {
        return publisher.score;
    }
    publisher
        .score
        .saturating_add(pow_bonus(effective_bits))
        .min(100)
}

/// Decode the `trust.root` setting (JSON string, tolerated raw) into a hex pubkey.
fn decode_root(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let s = match serde_json::from_str::<Option<String>>(&raw) {
        Ok(v) => v?,
        Err(_) => raw,
    };
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    crate::model::parse_pubkey(s).ok()
}

/// Graph data shared by every publisher scored in one pass.
struct Context {
    root: Option<String>,
    d1: Vec<String>,
    d1_set: HashSet<String>,
    /// {root} ∪ D1
    wot: Vec<String>,
    overrides: HashMap<String, OverrideAction>,
    root_mutes: HashSet<String>,
    d1_mutes: HashMap<String, Vec<String>>,
    reports: HashMap<String, Vec<(String, String)>>,
    graph_version: i64,
}

impl Context {
    async fn load<S: Store + ?Sized>(store: &S) -> crate::Result<Self> {
        let root = decode_root(store.get_setting(keys::TRUST_ROOT).await?);
        let overrides = store.overrides().await?.into_iter().collect();
        let graph_version = store.graph_version().await?;
        let mut ctx = Context {
            root: root.clone(),
            d1: Vec::new(),
            d1_set: HashSet::new(),
            wot: Vec::new(),
            overrides,
            root_mutes: HashSet::new(),
            d1_mutes: HashMap::new(),
            reports: HashMap::new(),
            graph_version,
        };
        let Some(root) = root else { return Ok(ctx) };

        let mut d1 = store.follows_of(&root).await?;
        d1.retain(|p| *p != root);
        d1.sort_unstable();
        d1.dedup();
        ctx.d1_set = d1.iter().cloned().collect();
        ctx.wot = std::iter::once(root.clone())
            .chain(d1.iter().cloned())
            .collect();
        for (muter, target) in store.mutes_by(&ctx.wot).await? {
            if muter == root {
                ctx.root_mutes.insert(target);
            } else if ctx.d1_set.contains(&muter) {
                ctx.d1_mutes.entry(target).or_default().push(muter);
            }
        }
        for (reporter, target, ty) in store.reports_by(&ctx.wot).await? {
            let ty = ty.to_ascii_lowercase();
            if REPORT_TYPES.contains(&ty.as_str()) {
                ctx.reports.entry(target).or_default().push((reporter, ty));
            }
        }
        ctx.d1 = d1;
        Ok(ctx)
    }

    async fn inputs<S: Store + ?Sized>(
        &self,
        store: &S,
        pubkey: &str,
        now: i64,
    ) -> crate::Result<TrustInputs> {
        let pubkey = pubkey.to_ascii_lowercase();
        let in_d1 = self.d1_set.contains(&pubkey);
        let is_root = self.root.as_deref() == Some(pubkey.as_str());
        let d1_followers = if self.d1.is_empty() || in_d1 || is_root {
            Vec::new()
        } else {
            store
                .followers_among(std::slice::from_ref(&pubkey), &self.d1)
                .await?
                .into_iter()
                .filter(|(t, _)| *t == pubkey)
                .flat_map(|(_, f)| f)
                .filter(|f| self.d1_set.contains(f))
                .collect()
        };
        let wot_commenters = if self.wot.is_empty() {
            Vec::new()
        } else {
            store.comment_authors_on(&pubkey, &self.wot).await?
        };
        let mut stats = store.publisher_stats(&pubkey).await?;
        stats.wot_comments = wot_commenters.iter().filter(|c| **c != pubkey).count() as u32;
        Ok(TrustInputs {
            override_action: self.overrides.get(&pubkey).copied(),
            root_muted: self.root_mutes.contains(&pubkey),
            in_d1,
            d1_followers,
            d1_muters: self.d1_mutes.get(&pubkey).cloned().unwrap_or_default(),
            wot_reports: self.reports.get(&pubkey).cloned().unwrap_or_default(),
            wot_commenters,
            stats,
            relay_list_oversized: store.relay_list_oversized(&pubkey).await?,
            root: self.root.clone(),
            now,
            graph_version: self.graph_version,
            pubkey,
        })
    }

    async fn score<S: Store + ?Sized>(
        &self,
        store: &S,
        pubkey: &str,
        now: i64,
    ) -> crate::Result<TrustScore> {
        let score = compute(&self.inputs(store, pubkey, now).await?);
        store.put_trust(&score).await?;
        Ok(score)
    }
}

/// Gather inputs from the store and score one publisher (stores the result).
pub async fn score_publisher<S: Store + ?Sized>(
    store: &S,
    pubkey: &str,
    now: i64,
) -> crate::Result<TrustScore> {
    Context::load(store).await?.score(store, pubkey, now).await
}

/// Upper bound on publishers rescored per pass.
const MAX_PUBLISHERS: usize = 1_000_000;

/// Rescore all publishers whose cached score is older than `max_age_secs` or from an older graph version.
pub async fn recompute<S: Store + ?Sized>(
    store: &S,
    now: i64,
    max_age_secs: i64,
) -> crate::Result<usize> {
    let ctx = Context::load(store).await?;
    let mut n = 0;
    for pk in store.publishers(MAX_PUBLISHERS).await? {
        let stale = match store.get_trust(&pk).await? {
            None => true,
            Some(s) => {
                s.graph_version != ctx.graph_version
                    || s.root != ctx.root
                    || s.computed_at < now - max_age_secs
            }
        };
        if stale {
            ctx.score(store, &pk, now).await?;
            n += 1;
        }
    }
    Ok(n)
}
