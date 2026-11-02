//! The fetch driver: rounds, byte budget, range caps, error propagation.

mod support;

use sniff::{Caps, Detector, Outcome, Range, SparseFile, detect_with};
use support::fixtures::*;

fn run(file: &[u8], name: &str, caps: &Caps) -> (sniff::Report, Vec<Range>) {
    let mut asked = Vec::new();
    let r = detect_with(
        |r: Range| -> Result<Vec<u8>, ()> {
            asked.push(r);
            let s = r.offset as usize;
            Ok(file[s..(s + r.len as usize).min(file.len())].to_vec())
        },
        file.len() as u64,
        Some(name),
        caps,
    )
    .unwrap();
    (r, asked)
}

#[test]
fn zip_needs_one_follow_up_round() {
    let apk = zip_store(&[
        ("a.bin", &vec![7u8; 300_000]),
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"x"),
    ]);
    let (r, asked) = run(&apk, "x.apk", &Caps::default());
    let Outcome::Detected(d) = &r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert_eq!(d.label, "package.apk");
    assert_eq!(r.rounds, 1);
    assert_eq!(asked[0], Range::new(0, 16384));
    assert!(r.bytes_fetched < 16384 + 70_000, "{}", r.bytes_fetched);
}

#[test]
fn round_cap_stops_and_flags_incomplete() {
    let apk = zip_store(&[
        ("a.bin", &vec![7u8; 300_000]),
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"x"),
    ]);
    let caps = Caps {
        rounds: 0,
        ..Caps::default()
    };
    let (r, asked) = run(&apk, "x.apk", &caps);
    assert_eq!(asked.len(), 1);
    let Outcome::Detected(d) = &r.outcome else {
        panic!()
    };
    assert!(d.flags.incomplete);
    assert!(!r.analysis.need.is_empty());
    // Review finding: the CLI prints `analysis.detection`, which never carried the flag.
    assert!(r.analysis.detection.flags.incomplete);
    assert_eq!(r.stop, sniff::Stop::Rounds);
}

#[test]
fn byte_budget_and_range_cap_are_respected() {
    let apk = zip_store(&[
        ("a.bin", &vec![7u8; 300_000]),
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"x"),
    ]);
    let caps = Caps {
        max_bytes: 20_000,
        max_range: 8192,
        ..Caps::default()
    };
    let (r, asked) = run(&apk, "x.apk", &caps);
    assert!(r.bytes_fetched <= 20_000);
    assert!(asked.iter().all(|a| a.len <= 16384));
    assert!(asked.iter().skip(1).all(|a| a.len <= 8192));
}

#[test]
fn head_and_tail_first_read() {
    let apk = zip_store(&[
        ("a.bin", &vec![7u8; 300_000]),
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"x"),
    ]);
    let caps = Caps {
        head: 4096,
        tail: 65536,
        ..Caps::default()
    };
    let (r, asked) = run(&apk, "x.apk", &caps);
    assert_eq!(r.rounds, 0);
    assert_eq!(asked.len(), 2);
    assert_eq!(
        r.outcome.detection().map(|d| d.label.as_str()),
        Some("package.apk")
    );
}

#[test]
fn fetch_errors_propagate() {
    let e = detect_with(
        |_r| -> Result<Vec<u8>, &str> { Err("peer gone") },
        1000,
        None,
        &Caps::default(),
    );
    assert_eq!(e.unwrap_err(), "peer gone");
}

#[test]
fn short_reads_end_the_rounds() {
    let r = detect_with(
        |_r| -> Result<Vec<u8>, ()> { Ok(Vec::new()) },
        1 << 30,
        Some("x.bin"),
        &Caps::default(),
    )
    .unwrap();
    assert!(r.rounds <= 1);
    assert_eq!(r.bytes_fetched, 0);
}

#[test]
fn empty_file() {
    let (r, _) = run(b"", "empty", &Caps::default());
    assert_eq!(
        r.outcome.detection().map(|d| d.label.as_str()),
        Some("misc.empty")
    );
}

/// Review finding: the driver inserted the whole buffer a fetch returned, so a peer answering
/// the 16 KiB head with 1 MiB planted an ISO PVD at 0x8000 (never asked for) that decided the
/// verdict, and kept bytes past the budget.
#[test]
fn oversized_fetch_answers_are_cut_to_the_request() {
    let len = 1u64 << 30;
    let mut handed = 0usize;
    let r = detect_with(
        |r: Range| -> Result<Vec<u8>, ()> {
            let mut b = vec![0u8; 1 << 20];
            put(&mut b, 0x8000, b"\x01CD001\x01");
            handed += b.len();
            if r.offset != 0 {
                b.truncate(r.len as usize);
            }
            Ok(b)
        },
        len,
        None,
        &Caps::default(),
    )
    .unwrap();
    assert!(handed >= 1 << 20);
    assert_ne!(r.analysis.detection.label, "disc.iso9660");
    let asked: u64 = r.fetched.iter().map(|x| x.len).sum();
    assert_eq!(r.bytes_fetched, asked);
}

fn vsize(n: u64) -> Vec<u8> {
    let mut v = n.to_be_bytes().to_vec();
    v[0] = 0x01;
    v
}

fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(vsize(body.len() as u64));
    v.extend_from_slice(body);
    v
}

/// A 4 GiB Matroska whose SeekHead points 16 times at Tracks headers of 2 MiB each.
fn many_tracks_pointers() -> SparseFile {
    let len: u64 = 4 << 30;
    let mut seekhead = Vec::new();
    let mut targets = Vec::new();
    for k in 0..16u64 {
        let pos = (k + 1) * (200 << 20) + 12345;
        let mut seek = el(&[0x53, 0xAB], &[0x16, 0x54, 0xAE, 0x6B]);
        seek.extend(el(&[0x53, 0xAC], &pos.to_be_bytes()));
        seekhead.extend(el(&[0x4D, 0xBB], &seek));
        targets.push(pos);
    }
    let ebml = el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska"));
    let mut head = ebml.clone();
    head.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
    head.extend(vsize(len - ebml.len() as u64 - 12));
    let seg_data = head.len() as u64;
    head.extend(el(&[0x11, 0x4D, 0x9B, 0x74], &seekhead));
    head.extend(el(&[0xEC], &vec![0u8; 16384 - head.len() - 9]));
    let mut f = SparseFile::new(len).with_name("x.mkv");
    f.insert(0, head);
    for p in &targets {
        let mut h = vec![0x16, 0x54, 0xAE, 0x6B];
        h.extend(vsize(2 << 20));
        f.insert(seg_data + p, h);
    }
    f
}

/// Review finding: one `NeedMore` could ask for max_ranges x max_range (~30 MiB, past the
/// 8 MiB budget), and `video::analyze` ignored max_ranges and, through block rounding,
/// max_range.
#[test]
fn need_more_is_held_to_the_caps() {
    let f = many_tracks_pointers();
    let caps = Caps::default();
    let Outcome::NeedMore(n) = Detector::new().detect(&f) else {
        panic!("expected NeedMore");
    };
    let total: u64 = n.iter().map(|r| r.len).sum();
    assert!(total <= caps.max_bytes, "{total}");
    assert!(n.len() <= caps.max_ranges);
    let r = sniff::video::analyze(&f, &caps);
    let total: u64 = r.need.iter().map(|r| r.len).sum();
    assert!(r.need.len() <= caps.max_ranges, "{}", r.need.len());
    assert!(total <= caps.max_bytes, "{total}");
    let small = Caps {
        max_range: 20_000,
        ..Caps::default()
    };
    let r = sniff::video::analyze(&f, &small);
    assert!(!r.need.is_empty());
    assert!(r.need.iter().all(|x| x.len <= 20_000), "{:?}", r.need);
}
