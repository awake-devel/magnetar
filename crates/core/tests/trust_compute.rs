//! Trust formula table (wot-relays.md §9).

use magnetar_core::model::{OverrideAction, PublisherStats, TrustScore};
use magnetar_core::trust::{
    TrustInputs, compute, event_trust, follow_d2_points, is_short_circuit, pow_bonus,
};

const NOW: i64 = 1_793_000_000;
const DAY: i64 = 86_400;

fn pk(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn base(p: u8) -> TrustInputs {
    TrustInputs {
        pubkey: pk(p),
        root: Some(pk(0xaa)),
        now: NOW,
        graph_version: 7,
        ..Default::default()
    }
}

fn codes(s: &TrustScore) -> Vec<(&str, i32)> {
    s.factors
        .iter()
        .map(|f| (f.code.as_str(), f.delta))
        .collect()
}

#[test]
fn nmirror_bridge_bot_pow21_gives_event_trust_20() {
    // b5c3145f… "nmirror": no WoT, no profile/relay list, 65/h max, proxy-tagged bridge.
    let mut i = base(1);
    i.stats = PublisherStats {
        first_seen: Some(NOW - 39 * 3600),
        torrent_count: 623,
        max_per_hour: 65,
        is_bridge: true,
        ..Default::default()
    };
    let s = compute(&i);
    assert_eq!(s.score, 0);
    assert!(s.factors.is_empty());
    assert_eq!(pow_bonus(21), 20);
    assert_eq!(event_trust(&s, 21), 20);

    // Even a real burst does not penalise a bridge.
    i.stats.max_per_hour = 500;
    assert_eq!(compute(&i).score, 0);
    assert!(compute(&i).factors.iter().all(|f| f.code != "BURST"));
}

#[test]
fn dd840e43_like_one_d2_follower_profile_age_is_21() {
    let mut i = base(2);
    i.d1_followers = vec![pk(3)];
    i.stats = PublisherStats {
        first_seen: Some(NOW - 400 * DAY),
        torrent_count: 78,
        has_profile: true,
        has_relay_list: true,
        ..Default::default()
    };
    let s = compute(&i);
    assert_eq!(s.score, 21);
    assert_eq!(
        codes(&s),
        vec![("FOLLOW_D2", 11), ("PROFILE", 5), ("AGE", 5)]
    );
    assert_eq!(s.factors[0].evidence, vec![pk(3)]);
    assert_eq!(s.factors[0].detail, "followed by 1 of your follows");
    assert_eq!(s.factors[2].detail, "first seen 400 days ago");
    assert_eq!(s.root, Some(pk(0xaa)));
    assert_eq!((s.computed_at, s.graph_version), (NOW, 7));
    // No PoW on hand-published events.
    assert_eq!(event_trust(&s, 0), 21);
}

#[test]
fn follow_d2_curve() {
    let table = [
        (0, 0),
        (1, 11),
        (2, 20),
        (3, 26),
        (5, 36),
        (10, 46),
        (40, 50),
    ];
    for (n2, pts) in table {
        assert_eq!(follow_d2_points(n2), pts, "n2={n2}");
    }
}

#[test]
fn pow_bonus_table() {
    let table = [
        (0, 0),
        (7, 0),
        (8, 0),
        (9, 1),
        (16, 12),
        (21, 20),
        (24, 25),
        (31, 25),
    ];
    for (bits, bonus) in table {
        assert_eq!(pow_bonus(bits), bonus, "bits={bits}");
    }
}

#[test]
fn d2_followers_deduped_and_self_ignored() {
    let mut i = base(2);
    i.d1_followers = vec![pk(3), pk(3), pk(2), pk(4), pk(5)];
    let s = compute(&i);
    assert_eq!(codes(&s), vec![("FOLLOW_D2", 26)]);
    assert_eq!(s.factors[0].evidence, vec![pk(3), pk(4), pk(5)]);
}

#[test]
fn root_and_positive_cap() {
    let mut i = base(0xaa);
    i.stats = PublisherStats {
        first_seen: Some(NOW - 90 * DAY),
        has_profile: true,
        has_relay_list: true,
        ..Default::default()
    };
    let s = compute(&i);
    assert_eq!(s.score, 100);
    assert_eq!(codes(&s), vec![("ROOT", 100)]);
    assert_eq!(s.factors.iter().map(|f| f.delta).sum::<i32>(), 100);

    // Root with a negative still subtracts from the capped positive sum.
    i.relay_list_oversized = true;
    let s = compute(&i);
    // PROFILE is gone with an oversized list; AGE is trimmed by the cap.
    assert_eq!(s.score, 85);
    assert_eq!(codes(&s), vec![("ROOT", 100), ("OVERSIZED_RELAYLIST", -15)]);
}

#[test]
fn d1_with_comments_capped_at_10() {
    let mut i = base(4);
    i.in_d1 = true;
    i.d1_followers = vec![pk(5)]; // ignored for D1 members
    i.wot_commenters = vec![pk(5), pk(6), pk(7), pk(8), pk(8), pk(4)];
    i.stats = PublisherStats {
        first_seen: Some(NOW - 31 * DAY),
        has_profile: true,
        has_relay_list: true,
        ..Default::default()
    };
    let s = compute(&i);
    assert_eq!(s.score, 90);
    assert_eq!(
        codes(&s),
        vec![
            ("FOLLOW_D1", 70),
            ("COMMENTS", 10),
            ("PROFILE", 5),
            ("AGE", 5)
        ]
    );
    let comments = &s.factors[1];
    assert_eq!(comments.evidence, vec![pk(5), pk(6), pk(7), pk(8)]);
    assert!(
        comments.detail.starts_with("4 people"),
        "{}",
        comments.detail
    );

    i.wot_commenters = vec![pk(5)];
    assert!(codes(&compute(&i)).contains(&("COMMENTS", 3)));
}

#[test]
fn age_needs_more_than_30_days() {
    let mut i = base(4);
    i.stats.first_seen = Some(NOW - 30 * DAY);
    assert!(compute(&i).factors.is_empty());
    i.stats.first_seen = Some(NOW - 30 * DAY - 1);
    assert_eq!(codes(&compute(&i)), vec![("AGE", 5)]);
}

#[test]
fn profile_needs_profile_and_sane_relay_list() {
    let mut i = base(4);
    i.stats.has_profile = true;
    assert!(compute(&i).factors.is_empty());
    i.stats.has_relay_list = true;
    assert_eq!(codes(&compute(&i)), vec![("PROFILE", 5)]);
    i.relay_list_oversized = true;
    assert_eq!(codes(&compute(&i)), vec![("OVERSIZED_RELAYLIST", -15)]);
}

#[test]
fn mutes_and_reports_capped() {
    let mut i = base(4);
    i.in_d1 = true;
    i.d1_muters = vec![pk(5), pk(6), pk(7), pk(8), pk(5)];
    i.wot_reports = vec![
        (pk(5), "spam".into()),
        (pk(5), "malware".into()), // same reporter counts once
        (pk(6), "illegal".into()),
        (pk(7), "nudity".into()), // not a counted type
        (pk(9), "spam".into()),
        (pk(10), "spam".into()),
        (pk(4), "spam".into()), // self-report ignored
    ];
    let s = compute(&i);
    assert_eq!(
        codes(&s),
        vec![("FOLLOW_D1", 70), ("MUTED_D1", -60), ("REPORTED", -45)]
    );
    assert_eq!(s.score, 0);
    let rep = &s.factors[2];
    assert_eq!(rep.evidence, vec![pk(5), pk(6), pk(9), pk(10)]);
    assert_eq!(
        rep.detail,
        "reported (illegal, malware, spam) by 4 people in your network"
    );

    i.d1_muters = vec![pk(5)];
    i.wot_reports = vec![(pk(6), "spam".into())];
    let s = compute(&i);
    assert_eq!(s.score, 35);
    assert_eq!(
        codes(&s),
        vec![("FOLLOW_D1", 70), ("MUTED_D1", -20), ("REPORTED", -15)]
    );
}

#[test]
fn burst_rules() {
    let mut i = base(4);
    i.stats.has_profile = true;
    i.stats.has_relay_list = true;
    i.stats.max_per_hour = 200;
    assert_eq!(codes(&compute(&i)), vec![("PROFILE", 5)]);
    i.stats.max_per_hour = 201;
    let s = compute(&i);
    assert_eq!(codes(&s), vec![("BURST", -10), ("PROFILE", 5)]);
    assert_eq!(s.score, 0);
    assert_eq!(
        s.factors[0].detail,
        "published 201 torrents within one hour"
    );
    i.stats.is_bridge = true;
    assert_eq!(codes(&compute(&i)), vec![("PROFILE", 5)]);
    i.stats.is_bridge = false;
    i.in_d1 = true;
    assert_eq!(compute(&i).score, 75);
}

#[test]
fn overrides_short_circuit() {
    let mut i = base(4);
    i.in_d1 = true;
    i.stats.has_profile = true;
    i.stats.has_relay_list = true;

    i.override_action = Some(OverrideAction::Block);
    let s = compute(&i);
    assert_eq!((s.score, codes(&s)), (0, vec![("BLOCKED", -100)]));
    assert!(is_short_circuit(&s));
    assert_eq!(event_trust(&s, 30), 0);

    i.override_action = Some(OverrideAction::Trust);
    i.d1_muters = vec![pk(5), pk(6), pk(7)];
    let s = compute(&i);
    assert_eq!((s.score, codes(&s)), (100, vec![("TRUSTED", 100)]));
    assert_eq!(event_trust(&s, 30), 100);

    // Root mute wins over WoT, but a local override wins over a root mute.
    i.override_action = None;
    i.root_muted = true;
    let s = compute(&i);
    assert_eq!((s.score, codes(&s)), (0, vec![("ROOT_MUTED", -100)]));
    assert_eq!(s.factors[0].evidence, vec![pk(0xaa)]);
    assert_eq!(event_trust(&s, 24), 0);
    i.override_action = Some(OverrideAction::Trust);
    assert_eq!(compute(&i).score, 100);
}

#[test]
fn no_root_only_local_signals() {
    let mut i = base(4);
    i.root = None;
    i.stats = PublisherStats {
        first_seen: Some(NOW - 100 * DAY),
        has_profile: true,
        has_relay_list: true,
        ..Default::default()
    };
    let s = compute(&i);
    assert_eq!(s.score, 10);
    assert_eq!(s.root, None);
    assert_eq!(event_trust(&s, 21), 30);
    i.override_action = Some(OverrideAction::Block);
    assert_eq!(compute(&i).score, 0);
}

#[test]
fn event_trust_clamps_at_100() {
    let mut i = base(4);
    i.in_d1 = true;
    i.wot_commenters = vec![pk(5), pk(6), pk(7), pk(8)];
    i.stats.has_profile = true;
    i.stats.has_relay_list = true;
    i.stats.first_seen = Some(0);
    let s = compute(&i);
    assert_eq!(s.score, 90);
    assert!(!is_short_circuit(&s));
    assert_eq!(event_trust(&s, 24), 100);
}

#[test]
fn every_factor_explains_itself() {
    let mut i = base(4);
    i.d1_followers = vec![pk(5), pk(6)];
    i.d1_muters = vec![pk(7)];
    i.wot_reports = vec![(pk(8), "spam".into())];
    i.wot_commenters = vec![pk(9)];
    i.relay_list_oversized = true;
    i.stats.max_per_hour = 999;
    i.stats.first_seen = Some(0);
    let s = compute(&i);
    assert_eq!(s.factors.len(), 7);
    for f in &s.factors {
        assert!(!f.detail.is_empty(), "{} has no detail", f.code);
    }
    let abs: Vec<i32> = s.factors.iter().map(|f| f.delta.abs()).collect();
    assert!(abs.windows(2).all(|w| w[0] >= w[1]), "sorted by |delta|");
    // 20 + 5 + 3 - 20 - 15 - 15 - 10 = -32 → clamped to 0.
    assert_eq!(s.score, 0);
}
