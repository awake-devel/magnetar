//! Hostile input: random, mutated and truncated bytes never panic (no parser panic is even
//! caught), and the driver stays within its round and byte budgets.

mod support;

use proptest::prelude::*;
use sniff::{Caps, Detector, Range, SparseFile, detect_with};
use support::fixtures::*;

/// Seeds that reach deep parser paths once mutated.
fn seeds() -> Vec<Vec<u8>> {
    vec![
        zip_store(&[("AndroidManifest.xml", b"x"), ("classes.dex", b"dex\n")]),
        mkv("matroska", &[(1, "V_MPEGH/ISO/HEVC"), (2, "A_AC3")]),
        mp4(b"isom", &[b"vide", b"soun"], 2000),
        tar_entry(".PKGINFO"),
        gzip(&tar_entry("a")),
        zstd(&tar_entry("a")),
        iso(
            "PLAYSTATION",
            "X",
            &[("SYSTEM.CNF", false, 22, 20)],
            &[(22, b"BOOT2 = cdrom0:\\A;1")],
        ),
        b"\x7FFIH\x01\x00\x03\0\0\0\0\0\0\0\0\0\0\0\x01\0\0\0\0\0".to_vec(),
        b"\x7FCNT\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x80".to_vec(),
        b"Rar!\x1a\x07\x01\x00\x03\x01\x00\x00".to_vec(),
        b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0".to_vec(),
        b"PFS0\x02\0\0\0\x20\0\0\0\0\0\0\0".to_vec(),
        b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1".to_vec(),
        b"xar!\0\x1c\0\x01\0\0\0\0\0\0\0\x40".to_vec(),
        b"ID3\x04\0\0\0\0\x10\x10\xFF\xFB\x90\x00".to_vec(),
        b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\x1e\x01vorbis".to_vec(),
        b"WEBVTT\n\n1\n00:00:01,000 --> 00:00:02,000\n".to_vec(),
    ]
}

fn check(bytes: Vec<u8>, size: u64, name: &str) {
    let before = sniff::panics_caught();
    let mut f = SparseFile::new(size.max(bytes.len() as u64)).with_name(name);
    f.insert(0, bytes);
    let det = Detector::new();
    let a = det.analyze(&f);
    let caps = Caps::default();
    for r in &a.need {
        assert!(
            r.offset < f.len() && r.len > 0 && r.len <= caps.max_range,
            "{r:?}"
        );
    }
    assert!(a.need.len() <= caps.max_ranges);
    let _ = det.finish(&f);
    assert_eq!(sniff::panics_caught(), before, "a parser panicked");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096), size in any::<u64>(), ext in 0usize..6) {
        let name = ["a.mkv", "a.zip", "a.pkg", "a", "a.iso", "a.exe"][ext];
        check(bytes, size, name);
    }

    #[test]
    fn mutated_seeds_never_panic(
        seed in 0usize..17,
        cut in any::<prop::sample::Index>(),
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..24),
        words in proptest::collection::vec((any::<prop::sample::Index>(), any::<u32>()), 0..6),
        size_kind in 0u8..3,
    ) {
        let mut b = seeds()[seed % 17].clone();
        for (i, v) in flips {
            let i = i.index(b.len());
            b[i] = v;
        }
        for (i, v) in words {
            if b.len() >= 4 {
                let i = i.index(b.len() - 3);
                b[i..i + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        let n = b.len();
        b.truncate(cut.index(n + 1));
        let size = match size_kind {
            0 => n as u64,
            1 => b.len() as u64,
            _ => u64::MAX / 3,
        };
        check(b, size, "x.bin");
    }

    #[test]
    fn driver_stays_within_budget(
        bytes in proptest::collection::vec(any::<u8>(), 0..2048),
        len in 0u64..(1 << 40),
        rounds in 0u32..4,
        max_bytes in 1u64..200_000,
    ) {
        let caps = Caps { rounds, max_bytes, ..Caps::default() };
        let mut asked: Vec<Range> = Vec::new();
        let r = detect_with(
            |r: Range| -> Result<Vec<u8>, ()> {
                asked.push(r);
                // Serve the random bytes at offset 0, zeros elsewhere (bounded allocations).
                let n = r.len.min(1 << 20) as usize;
                let mut v = vec![0u8; n];
                if r.offset == 0 {
                    let k = n.min(bytes.len());
                    v[..k].copy_from_slice(&bytes[..k]);
                }
                Ok(v)
            },
            len,
            Some("x.mkv"),
            &caps,
        ).unwrap();
        prop_assert!(r.rounds <= rounds);
        prop_assert!(r.bytes_fetched <= max_bytes);
        for a in &asked {
            prop_assert!(a.end() <= len && a.len <= caps.max_range.max(caps.head));
        }
    }
}
