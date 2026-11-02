//! The two local PS5 packages again (ranges read in place, never copied), for the id schema
//! added on top of tests/ps5_samples.rs: database id, content kind, SDK, the PS4 title IDs the
//! save data transfers from, and a split package read through `listing::split_range`. Skips
//! when a sample is missing (any other machine).

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use sniff::listing::{classify, split_range};
use sniff::{Caps, Content, Range, Value, detect_with};

const HZR: &str = "/home/dude/Téléchargements/HZR.pkg";
const SILKSONG: &str = "/home/dude/Téléchargements/PPSA12544-Hollow Knight Silksong.pkg";

fn open(path: &str) -> Option<(File, u64)> {
    if !Path::new(path).is_file() {
        eprintln!("note: {path} is missing; skipping");
        return None;
    }
    let f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    Some((f, len))
}

fn read(f: &File, off: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut b = vec![0u8; len as usize];
    f.read_exact_at(&mut b, off)?;
    Ok(b)
}

fn check(path: &str, db_id: &str, sdk: Option<&str>, ps4: &str, created: Option<&str>) {
    let Some((f, len)) = open(path) else { return };
    let r = detect_with(
        |r: Range| read(&f, r.offset, r.len),
        len,
        Some(path),
        &Caps::default(),
    )
    .unwrap();
    let d = r.outcome.detection().unwrap();
    let g = d.game.as_ref().unwrap();
    assert_eq!(g.db_id.as_deref(), Some(db_id));
    assert_eq!(g.content, Some(Content::Game));
    assert_eq!(g.sdk.as_deref(), sdk);
    assert_eq!(g.encrypted, None);
    match d.field("ps4_title_ids") {
        Some(Value::List(l)) => assert!(l.iter().any(|x| x == ps4), "{l:?}"),
        other => panic!("{other:?}"),
    }
    assert!(r.bytes_fetched <= 81920, "{}", r.bytes_fetched);
    // HZR's param.json says 1970-01-01 00:00:00 (unset): no date rather than a fake one.
    assert_eq!(d.field_str("creation_date"), created);

    // The same package split in parts of at most 4 GiB (FAT32 style `_N.pkg`, at least three
    // parts): the listing orders the parts and split_range maps the driver's ranges onto them.
    let part = (4u64 << 30).min(len.div_ceil(3));
    let n = len.div_ceil(part);
    let names: Vec<String> = (0..n)
        .rev()
        .map(|i| format!("Game/{db_id}_{i}.pkg"))
        .collect();
    let sizes: Vec<(String, u64)> = names
        .iter()
        .map(|p| {
            let i: u64 = p
                .rsplit('_')
                .next()
                .unwrap()
                .trim_end_matches(".pkg")
                .parse()
                .unwrap();
            (p.clone(), (len - i * part).min(part))
        })
        .collect();
    let ls = classify(sizes.iter().map(|(p, s)| (p.as_str(), *s)));
    let l = ls.iter().find(|l| l.label == "game.pkg.split").unwrap();
    assert_eq!((l.total_size, l.parts.len() as u64), (len, n));
    assert_eq!(l.title_id.as_deref(), Some(db_id));
    let part_sizes: Vec<u64> = l
        .parts
        .iter()
        .map(|p| sizes.iter().find(|(q, _)| q == p).unwrap().1)
        .collect();
    let mut reads = 0usize;
    let split = detect_with(
        |r: Range| -> std::io::Result<Vec<u8>> {
            let mut out = Vec::new();
            for (i, off, n) in split_range(&part_sizes, r.offset, r.len) {
                reads += 1;
                // Part i of the virtual split is the real file at i * part.
                out.extend(read(&f, i as u64 * part + off, n)?);
            }
            Ok(out)
        },
        l.total_size,
        Some(&l.parts[0]),
        &Caps::default(),
    )
    .unwrap();
    assert!(reads >= 2);
    let ds = split.outcome.detection().unwrap();
    assert_eq!(
        (ds.label.as_str(), ds.confidence),
        (d.label.as_str(), d.confidence)
    );
    assert_eq!(ds.game, d.game);
}

#[test]
fn horizon_zero_dawn_remastered_ids() {
    check(HZR, "PPSA13427", Some("10.00"), "CUSA01021", None);
}

#[test]
fn hollow_knight_silksong_ids() {
    // sdkVersion is 0x0000000000000000 in this package: no SDK reported.
    check(
        SILKSONG,
        "PPSA12544",
        None,
        "CUSA39420",
        Some("2026-10-01 06:04:54"),
    );
}
