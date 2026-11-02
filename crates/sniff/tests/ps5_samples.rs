//! Against the two local PS5 packages: reads only the ranges the detector asks for, in place
//! (never copied). Skips with a note when a sample is missing (any other machine).

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use sniff::{Caps, Detector, GameKind, Outcome, Platform, Range, SparseFile, detect_with};

const HZR: &str = "/home/dude/Téléchargements/HZR.pkg";
const SILKSONG: &str = "/home/dude/Téléchargements/PPSA12544-Hollow Knight Silksong.pkg";
const WIN_ISO: &str = "/home/dude/Downloads/Windows11_Client_x64_fr-fr_26300_9457.iso";

struct Expect {
    content_id: &'static str,
    title_id: &'static str,
    title: &'static str,
    version: &'static str,
    fw: &'static str,
    cnt: u64,
}

fn open(path: &str) -> Option<(File, u64)> {
    if !Path::new(path).is_file() {
        eprintln!("note: {path} is missing; skipping");
        return None;
    }
    let f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    Some((f, len))
}

fn read(f: &File, r: Range) -> std::io::Result<Vec<u8>> {
    let mut b = vec![0u8; r.len as usize];
    f.read_exact_at(&mut b, r.offset)?;
    Ok(b)
}

fn check(path: &str, e: &Expect) {
    let Some((f, len)) = open(path) else { return };
    // Driver with the default caps: 16 KiB head, then the CNT block.
    let r = detect_with(|r| read(&f, r), len, Some(path), &Caps::default()).unwrap();
    let Outcome::Detected(d) = &r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps5.pkg", 99));
    assert_eq!(
        r.fetched,
        vec![Range::new(0, 16384), Range::new(e.cnt, 0x10000)]
    );
    let g = d.game.as_ref().unwrap();
    assert_eq!(
        (g.platform, g.kind, g.format, g.debug),
        (Platform::Ps5, GameKind::Package, "PS5 FIH", Some(true))
    );
    assert_eq!(g.content_id.as_deref(), Some(e.content_id));
    assert_eq!(g.title_id.as_deref(), Some(e.title_id));
    assert_eq!(g.title.as_deref(), Some(e.title));
    assert_eq!(g.version.as_deref(), Some(e.version));
    assert_eq!(g.required_fw.as_deref(), Some(e.fw));
    assert_eq!(g.region, Some("Europe/PAL"));
    assert_eq!(d.field_str("plain_marker"), Some("PPRPLAIN-NOAUTH!"));

    // The first 256 bytes alone: the label and the CNT request.
    let mut s = SparseFile::new(len);
    s.insert(0, read(&f, Range::new(0, 0x100)).unwrap());
    assert_eq!(
        Detector::new().detect(&s),
        Outcome::NeedMore(vec![Range::new(e.cnt, 0x10000)])
    );

    // Content ID from the last MiB alone (trailing PlayGo ZIP), before the CNT is fetched.
    let tail = 1u64 << 20;
    s.insert(len - tail, read(&f, Range::new(len - tail, tail)).unwrap());
    let a = Detector::new().analyze(&s);
    assert_eq!(
        a.detection
            .game
            .as_ref()
            .and_then(|g| g.content_id.as_deref()),
        Some(e.content_id)
    );
    assert_eq!(
        a.detection.game.as_ref().and_then(|g| g.title.as_deref()),
        None
    );

    // A wrong file length (another file's) is not a confident PS5 package.
    let mut wrong = SparseFile::new(e.cnt);
    wrong.insert(0, read(&f, Range::new(0, 0x100)).unwrap());
    assert!(Detector::new().analyze(&wrong).detection.confidence < 80);
}

#[test]
fn hzr() {
    check(
        HZR,
        &Expect {
            content_id: "EP9000-PPSA13427_00-HORIZONREMASTER1",
            title_id: "PPSA13427",
            title: "Horizon Zero Dawn\u{2122} Remastered",
            version: "01.008.001",
            fw: "10.00",
            cnt: 0x15af980000,
        },
    );
}

#[test]
fn silksong() {
    check(
        SILKSONG,
        &Expect {
            content_id: "EP1805-PPSA12544_00-HKSILKSONGPS5000",
            title_id: "PPSA12544",
            title: "Hollow Knight: Silksong",
            version: "01.000.010",
            fw: "9.00",
            cnt: 0x8c480000,
        },
    );
}

#[test]
fn a_damaged_fih_is_not_a_fact() {
    let Some((f, len)) = open(SILKSONG) else {
        return;
    };
    let head = read(&f, Range::new(0, 0x100)).unwrap();
    for (off, val) in [
        (0x18usize, 0xffu8),
        (0x5d, 0x7f),
        (0x05, 0x01),
        (0x12, 0x00),
    ] {
        let mut h = head.clone();
        h[off] = val;
        let mut s = SparseFile::new(len);
        s.insert(0, h);
        assert!(
            Detector::new().analyze(&s).detection.confidence < 80,
            "byte {off:#x} = {val:#x}"
        );
    }
}

#[test]
fn a_windows_iso_is_not_a_console_image() {
    let Some((f, len)) = open(WIN_ISO) else {
        return;
    };
    let r = detect_with(|r| read(&f, r), len, Some(WIN_ISO), &Caps::default()).unwrap();
    let d = r.analysis.detection;
    assert!(!d.label.starts_with("game."), "{}", d.label);
    assert!(d.game.is_none());
}
