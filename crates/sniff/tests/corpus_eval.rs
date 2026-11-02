//! Accuracy on the prototype's corpora (dev and holdout manifests). Skipped when the corpora
//! are not on this machine. Thresholds: the prototype's results (holdout 100% exact, dev
//! 306/307 = 99.7%; the one dev miss is a .cb7 whose file list is LZMA-compressed).

mod support;

use sniff::Caps;
use support::eval;

fn score(manifest: &str, caps: &Caps, min_exact_pct: f64) {
    let Some(m) = eval::manifest(manifest) else {
        eprintln!(
            "note: {manifest} not found under {}; skipping",
            eval::CORPUS_DIR
        );
        return;
    };
    let s = eval::run(&m, caps);
    for r in s.misses() {
        eprintln!("MISS {} want {} got {}", r.path, r.want, r.got);
    }
    eprintln!(
        "{manifest}: {}/{} ({:.1}%), head {} tail {} rounds {}",
        s.exact(),
        s.rows.len(),
        s.pct(),
        caps.head,
        caps.tail,
        caps.rounds
    );
    assert_eq!(s.missing, 0, "files listed in {manifest} are missing");
    assert!(
        s.pct() >= min_exact_pct,
        "{manifest}: {:.2}% < {min_exact_pct}%",
        s.pct()
    );
    assert_eq!(sniff::panics_caught(), 0);
}

#[test]
fn holdout_default_caps() {
    score("manifest_holdout.tsv", &Caps::default(), 100.0);
}

#[test]
fn holdout_prototype_caps() {
    score("manifest_holdout.tsv", &eval::proto_caps(), 100.0);
}

#[test]
fn dev_default_caps() {
    score("manifest.tsv", &Caps::default(), 99.6);
}

#[test]
fn dev_prototype_caps() {
    score("manifest.tsv", &eval::proto_caps(), 99.6);
}
