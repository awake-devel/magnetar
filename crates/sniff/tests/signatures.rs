//! The TOML signature engine: built-in files compile and only use registered labels; user
//! rules exercise continuation levels, typed extraction, indirect / relative / end-of-file
//! offsets, search windows, optional children, overrides and strength ordering.

use sniff::{Detector, Outcome, Signatures, SparseFile, Value, label};

#[test]
fn builtin_signatures_compile_and_use_registered_labels() {
    let s = Signatures::try_builtin().expect("built-in signatures compile");
    assert!(s.len() > 80, "{}", s.len());
    for r in s.info() {
        assert!(
            label::info(&r.label).is_some(),
            "rule {} uses unregistered label {}",
            r.id,
            r.label
        );
    }
    assert_eq!(Signatures::builtin().len(), s.len());
    // Overrides inside the built-in files are registered too.
    for (name, text) in sniff::magic::BUILTIN_FILES {
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("label = \"") {
                let id = v.trim_end_matches('"');
                assert!(label::info(id).is_some(), "{name}: {id}");
            }
        }
    }
}

const USER: &str = r#"
[[rule]]
id = "demo"
label = "misc.demo"
mime = "application/x-demo"
description = "Demo container"
confidence = 90
type = "string"
value = 'DEMO\x01'
  [[rule.then]]                      # u32le pointer at 8 -> "BODY" there
  offset = { at = 8, type = "u32le" }
  type = "string"
  value = "BODY"
    [[rule.then.then]]               # right after BODY: u16be version
    offset = 0
    relative = true
    type = "u16be"
    field = "version"
  [[rule.then]]
  offset = 12
  type = "cstring"
  len = 16
  field = "title"
  [[rule.then]]                      # optional trailer at EOF-4 upgrades the label
  offset = -4
  type = "string"
  value = "END!"
  optional = true
  label = "misc.demo_full"
  confidence = 97
  [[rule.then]]                      # optional search for a marker in the first 256 bytes
  offset = 0
  type = "search"
  range = 256
  value = "MARK"
  field = "mark_at"
  optional = true
"#;

fn demo(trailer: bool) -> Vec<u8> {
    let mut b = vec![0u8; 128];
    b[..5].copy_from_slice(b"DEMO\x01");
    b[8..12].copy_from_slice(&64u32.to_le_bytes());
    b[12..17].copy_from_slice(b"Hello");
    b[64..68].copy_from_slice(b"BODY");
    b[68..70].copy_from_slice(&0x0203u16.to_be_bytes());
    b[90..94].copy_from_slice(b"MARK");
    if trailer {
        b[124..128].copy_from_slice(b"END!");
    }
    b
}

#[test]
fn user_rules_cover_the_engine_features() {
    let sigs = Signatures::with_user("user.toml", USER).unwrap();
    assert_eq!(sigs.len(), Signatures::builtin().len() + 1);
    let det = Detector::with_signatures(&sigs);
    let f = SparseFile::from_bytes(demo(false));
    let Outcome::Detected(d) = det.detect(&f) else {
        panic!()
    };
    assert_eq!(
        (d.label.as_str(), d.confidence, d.mime.as_str()),
        ("misc.demo", 90, "application/x-demo")
    );
    assert_eq!(d.field("version"), Some(&Value::U64(0x0203)));
    assert_eq!(d.field_str("title"), Some("Hello"));
    assert_eq!(d.field("mark_at"), Some(&Value::U64(90)));
    assert_eq!(d.candidates[0].source, "demo");
    let Outcome::Detected(d) = det.detect(&SparseFile::from_bytes(demo(true))) else {
        panic!()
    };
    assert_eq!((d.label.as_str(), d.confidence), ("misc.demo_full", 97));
    // The pointed-to block is required: a wrong pointer fails the rule.
    let mut bad = demo(false);
    bad[8..12].copy_from_slice(&32u32.to_le_bytes());
    assert_ne!(
        det.analyze(&SparseFile::from_bytes(bad)).detection.label,
        "misc.demo"
    );
}

#[test]
fn missing_continuation_bytes_give_a_partial_match_that_asks_for_them() {
    let sigs = Signatures::with_user("user.toml", USER).unwrap();
    let det = Detector::with_signatures(&sigs);
    let full = demo(false);
    let mut f = SparseFile::new(full.len() as u64);
    f.insert(0, full[..32].to_vec());
    let a = det.analyze(&f);
    // Only the 5-byte magic checked so far: a hint (79), less the partial penalty (20).
    assert_eq!(
        (a.detection.label.as_str(), a.detection.confidence),
        ("misc.demo", 59)
    );
    assert!(
        a.need.iter().any(|r| r.offset <= 64 && r.end() >= 68),
        "{:?}",
        a.need
    );
}

#[test]
fn bad_user_files_are_errors_not_panics() {
    for bad in [
        "not toml [[",
        "[[rule]]\nlabel = \"x\"\nconfidence = 101\ntype = \"u8\"",
        "[[rule]]\nlabel = \"x\"\nconfidence = 1\ntype = \"u8\"\ntest = \">=zz\"",
        "[[rule]]\nlabel = \"x\"\nconfidence = 1\ntype = \"search\"\nvalue = \"a\"",
        "[[rule]]\nlabel = \"x\"\nconfidence = 1\ntype = \"string\"\nvalue = '\\xZZ'",
    ] {
        let e = Signatures::from_toml("bad.toml", bad).unwrap_err();
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn strength_orders_equal_confidences() {
    let text = r#"
[[rule]]
id = "weak"
label = "misc.a"
confidence = 90
strength = 1
type = "string"
value = "XYZ"
[[rule]]
id = "strong"
label = "misc.b"
confidence = 90
strength = 50
type = "string"
value = "XY"
"#;
    let sigs = Signatures::from_toml("t.toml", text).unwrap();
    let d = Detector::with_signatures(&sigs)
        .analyze(&SparseFile::from_bytes(b"XYZ!".to_vec()))
        .detection;
    assert_eq!(d.label, "misc.b");
}

#[test]
fn rules_report_their_head_and_tail_needs() {
    let s = Signatures::builtin();
    let info = s.info();
    let ncz = info.iter().find(|r| r.id == "switch_ncz").unwrap();
    assert_eq!(ncz.head, 0x4008);
    let dmg = info.iter().find(|r| r.id == "dmg").unwrap();
    assert_eq!(dmg.tail, 512);
    assert!(s.max_tail() >= 512);
}

/// Review finding: the gzip/zstd/zlib peek labelled its payload with the built-in rules only,
/// so a user rule gave a different label to the same bytes once compressed.
#[test]
fn compressed_peek_uses_the_detectors_signatures() {
    let user = r#"
[[rule]]
id = "qqzz"
label = "custom.thing"
confidence = 90
type = "string"
value = 'QQZZ\x01\x02'
"#;
    let sigs = sniff::Signatures::with_user("user.toml", user).unwrap();
    let det = sniff::Detector::with_signatures(&sigs);
    let mut payload = b"QQZZ\x01\x02".to_vec();
    payload.extend(vec![7u8; 4000]);
    let plain = sniff::SparseFile::from_bytes(payload.clone());
    assert_eq!(det.analyze(&plain).detection.label, "custom.thing");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &payload).unwrap();
    let gz = sniff::SparseFile::from_bytes(gz.finish().unwrap());
    let d = det.analyze(&gz).detection;
    assert_eq!(d.label, "archive.gzip");
    assert_eq!(d.field_str("inner"), Some("custom.thing"));
}
