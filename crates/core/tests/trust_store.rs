//! trust::score_publisher / recompute over a Store.

mod trust_support;

use magnetar_core::model::{OverrideAction, PublisherStats, npub};
use magnetar_core::store::{Store, keys};
use magnetar_core::trust::{recompute, score_publisher};
use trust_support::MemStore;

const NOW: i64 = 1_793_000_000;

fn pk(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// Root R=aa follows A=01, B=02, C=03. A and B follow P=10. C mutes Q=11 and B reports Q (spam).
/// R mutes M=12. Stranger S=99 (not in WoT) reports P and mutes P. A commented on P.
fn graph() -> MemStore {
    let s = MemStore::default();
    s.with(|i| {
        i.settings.insert(
            keys::TRUST_ROOT.into(),
            serde_json::to_string(&pk(0xaa)).unwrap(),
        );
        i.follows
            .insert(pk(0xaa), vec![pk(1), pk(2), pk(3), pk(0xaa)]);
        i.follows.insert(pk(1), vec![pk(0x10), pk(0x11)]);
        i.follows.insert(pk(2), vec![pk(0x10)]);
        i.follows.insert(pk(0x99), vec![pk(0x10)]);
        i.mutes = vec![
            (pk(3), pk(0x11)),
            (pk(0xaa), pk(0x12)),
            (pk(0x99), pk(0x10)),
        ];
        i.reports = vec![
            (pk(2), pk(0x11), "SPAM".into()),
            (pk(0x99), pk(0x10), "spam".into()),
            (pk(3), pk(0x10), "profanity".into()),
        ];
        i.comments = vec![(pk(0x10), pk(1)), (pk(0x10), pk(0x99))];
        i.stats.insert(
            pk(0x10),
            PublisherStats {
                first_seen: Some(NOW - 60 * 86_400),
                has_profile: true,
                has_relay_list: true,
                ..Default::default()
            },
        );
        i.publishers = vec![pk(0x10), pk(0x11), pk(0x12), pk(1), pk(0xaa)];
        i.graph_version = 3;
    });
    s
}

#[tokio::test]
async fn scores_from_graph() {
    let s = graph();
    let p = score_publisher(&s, &pk(0x10), NOW).await.unwrap();
    // n2 = 2 → 20, COMMENTS 3 (only A), PROFILE 5, AGE 5; stranger signals ignored.
    let codes: Vec<(&str, i32)> = p
        .factors
        .iter()
        .map(|f| (f.code.as_str(), f.delta))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("FOLLOW_D2", 20),
            ("PROFILE", 5),
            ("AGE", 5),
            ("COMMENTS", 3)
        ]
    );
    assert_eq!(p.score, 33);
    assert_eq!(p.factors[0].evidence, vec![pk(1), pk(2)]);
    assert_eq!(p.factors[3].evidence, vec![pk(1)]);
    assert_eq!((p.root.clone(), p.graph_version), (Some(pk(0xaa)), 3));
    assert_eq!(s.get_trust(&pk(0x10)).await.unwrap(), Some(p));

    let q = score_publisher(&s, &pk(0x11), NOW).await.unwrap();
    let codes: Vec<(&str, i32)> = q
        .factors
        .iter()
        .map(|f| (f.code.as_str(), f.delta))
        .collect();
    assert_eq!(
        codes,
        vec![("MUTED_D1", -20), ("REPORTED", -15), ("FOLLOW_D2", 11)]
    );
    assert_eq!(q.score, 0);

    let m = score_publisher(&s, &pk(0x12), NOW).await.unwrap();
    assert_eq!((m.score, m.factors[0].code.as_str()), (0, "ROOT_MUTED"));

    let a = score_publisher(&s, &pk(1), NOW).await.unwrap();
    assert_eq!((a.score, a.factors[0].code.as_str()), (70, "FOLLOW_D1"));

    let r = score_publisher(&s, &pk(0xaa), NOW).await.unwrap();
    assert_eq!((r.score, r.factors[0].code.as_str()), (100, "ROOT"));
}

#[tokio::test]
async fn root_setting_accepts_npub_and_uppercase_input_pubkey() {
    let s = graph();
    s.set_setting(
        keys::TRUST_ROOT,
        &serde_json::to_string(&npub(&pk(0xaa))).unwrap(),
    )
    .await
    .unwrap();
    let p = score_publisher(&s, &pk(0x10).to_uppercase(), NOW)
        .await
        .unwrap();
    assert_eq!(p.pubkey, pk(0x10));
    assert_eq!(p.score, 33);
    // A raw (non-JSON) value is tolerated too.
    s.set_setting(keys::TRUST_ROOT, &pk(0xaa)).await.unwrap();
    assert_eq!(score_publisher(&s, &pk(0x10), NOW).await.unwrap().score, 33);
}

#[tokio::test]
async fn no_root_only_overrides_and_local_signals() {
    let s = graph();
    s.with(|i| i.settings.clear());
    let p = score_publisher(&s, &pk(0x10), NOW).await.unwrap();
    assert_eq!(p.root, None);
    assert_eq!(p.score, 10, "{:?}", p.factors);
    // JSON null also means "no root".
    s.set_setting(keys::TRUST_ROOT, "null").await.unwrap();
    assert_eq!(score_publisher(&s, &pk(0x12), NOW).await.unwrap().score, 0);

    s.set_override(&pk(0x12), Some(OverrideAction::Trust))
        .await
        .unwrap();
    let m = score_publisher(&s, &pk(0x12), NOW).await.unwrap();
    assert_eq!((m.score, m.factors[0].code.as_str()), (100, "TRUSTED"));
}

#[tokio::test]
async fn override_beats_wot() {
    let s = graph();
    s.set_override(&pk(1), Some(OverrideAction::Block))
        .await
        .unwrap();
    let a = score_publisher(&s, &pk(1), NOW).await.unwrap();
    assert_eq!((a.score, a.factors.len()), (0, 1));
    assert_eq!(a.factors[0].code, "BLOCKED");
}

#[tokio::test]
async fn recompute_only_stale() {
    let s = graph();
    assert_eq!(recompute(&s, NOW, 3600).await.unwrap(), 5);
    assert_eq!(s.with(|i| i.put_trust_calls), 5);
    // Fresh, same graph: nothing to do.
    assert_eq!(recompute(&s, NOW + 60, 3600).await.unwrap(), 0);
    // Too old.
    assert_eq!(recompute(&s, NOW + 7200, 3600).await.unwrap(), 5);
    // Graph changed.
    s.bump_graph_version().await.unwrap();
    assert_eq!(recompute(&s, NOW + 7300, 3600).await.unwrap(), 5);
    assert_eq!(s.get_trust(&pk(1)).await.unwrap().unwrap().graph_version, 4);
    // Root changed: every cached score is for another root.
    s.set_setting(keys::TRUST_ROOT, &serde_json::to_string(&pk(1)).unwrap())
        .await
        .unwrap();
    assert_eq!(recompute(&s, NOW + 7400, 3600).await.unwrap(), 5);
    assert_eq!(s.get_trust(&pk(1)).await.unwrap().unwrap().score, 100);
    assert_eq!(recompute(&s, NOW + 7500, 3600).await.unwrap(), 0);
}
