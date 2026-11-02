//! False positives of the TOML rules on misplaced windows, as the prototype's fp mode: bytes
//! that are not the start of a file must not make any signature rule fire at a storable
//! confidence (80+).
//!
//! - 2000 synthetic windows (uniform random, ASCII text, UTF-16 text, small-integer binary
//!   tables, sparse data), always run.
//! - 2000 windows of 64 KiB from the middle of the prototype corpus files when the corpus is
//!   on this machine (skipped otherwise).

mod support;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use sniff::{Detector, Signatures, SparseFile};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const WORDS: &[&str] = &[
    "the", "of", "and", "PACK", "SEGA", "Vgm", "GBS", "data", "file", "IMPM", "0x7F", "lorem",
    "ipsum", "MThd", "1080p", "x264", "BluRay", "DTS-HD", "Season", "Episode", "glTF", "PSF",
];

fn window(kind: u64, r: &mut Rng, len: usize) -> Vec<u8> {
    match kind % 5 {
        0 => (0..len).map(|_| r.next() as u8).collect(),
        1 => {
            let mut s = String::new();
            while s.len() < len {
                s.push_str(WORDS[r.below(WORDS.len() as u64) as usize]);
                s.push(if r.below(12) == 0 { '\n' } else { ' ' });
                if r.below(9) == 0 {
                    s.push_str(&r.below(100_000).to_string());
                }
            }
            s.into_bytes()[..len].to_vec()
        }
        2 => {
            let mut s: Vec<u8> = Vec::new();
            while s.len() < len {
                let w = WORDS[r.below(WORDS.len() as u64) as usize];
                for u in w.encode_utf16().chain(" ".encode_utf16()) {
                    s.extend_from_slice(&u.to_le_bytes());
                }
            }
            s.truncate(len);
            s
        }
        3 => {
            // Tables of small little-endian integers (offsets, counts, sizes).
            let mut s = Vec::with_capacity(len);
            while s.len() < len {
                let v = match r.below(4) {
                    0 => r.below(16) as u32,
                    1 => r.below(4096) as u32,
                    2 => r.below(1 << 20) as u32,
                    _ => r.next() as u32,
                };
                s.extend_from_slice(&v.to_le_bytes());
            }
            s.truncate(len);
            s
        }
        _ => (0..len)
            .map(|_| if r.below(8) == 0 { r.next() as u8 } else { 0 })
            .collect(),
    }
}

/// TOML-rule candidates at 80+ in a pass over `f` (rule id, label, confidence).
fn confident_rule_hits(det: &Detector, ids: &[String], f: &SparseFile) -> Vec<String> {
    det.analyze(f)
        .detection
        .candidates
        .iter()
        .filter(|c| c.confidence >= 80 && ids.contains(&c.source))
        .map(|c| format!("{} -> {} {}", c.source, c.label, c.confidence))
        .collect()
}

#[test]
fn rules_do_not_fire_on_2000_synthetic_windows() {
    let ids: Vec<String> = Signatures::builtin()
        .info()
        .into_iter()
        .map(|r| r.id)
        .collect();
    let det = Detector::new();
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut hits = Vec::new();
    for i in 0..2000u64 {
        let w = window(i, &mut r, 16 * 1024);
        // As if the window were the start of a file of plausible size (head known only).
        let len = 16 * 1024 + r.below(64 << 20);
        let mut f = SparseFile::new(len);
        f.insert(0, w);
        for h in confident_rule_hits(&det, &ids, &f) {
            hits.push(format!("window {i} (kind {}): {h}", i % 5));
        }
    }
    assert!(hits.is_empty(), "{} hits:\n{}", hits.len(), hits.join("\n"));
    assert_eq!(sniff::panics_caught(), 0);
}

#[test]
fn rules_do_not_fire_on_2000_corpus_windows() {
    let Some(manifest) = support::eval::manifest("manifest.tsv") else {
        eprintln!(
            "note: corpus not found under {}; skipping",
            support::eval::CORPUS_DIR
        );
        return;
    };
    let dir = manifest.parent().unwrap().to_path_buf();
    let text = std::fs::read_to_string(&manifest).unwrap();
    let mut files: Vec<(std::path::PathBuf, u64)> = text
        .lines()
        .filter_map(|l| l.split('\t').next())
        .filter(|p| !p.is_empty() && !p.starts_with('#'))
        .map(|p| dir.join(p))
        .filter_map(|p| {
            let len = std::fs::metadata(&p).ok()?.len();
            (len >= 300_000).then_some((p, len))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let ids: Vec<String> = Signatures::builtin()
        .info()
        .into_iter()
        .map(|r| r.id)
        .collect();
    let det = Detector::new();
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let (mut n, mut hits) = (0usize, Vec::new());
    'outer: loop {
        for (p, size) in &files {
            if n >= 2000 {
                break 'outer;
            }
            let Ok(mut f) = File::open(p) else { continue };
            let off = 65536 + r.below(size - 200_000);
            let mut b = Vec::new();
            if f.seek(SeekFrom::Start(off)).is_err()
                || (&mut f).take(65536).read_to_end(&mut b).is_err()
            {
                continue;
            }
            let mut s = SparseFile::new(size - off);
            s.insert(0, b);
            n += 1;
            for h in confident_rule_hits(&det, &ids, &s) {
                hits.push(format!("{}@{off}: {h}", p.display()));
            }
        }
    }
    eprintln!("corpus windows: {n}, confident rule hits: {}", hits.len());
    assert!(hits.is_empty(), "{} hits:\n{}", hits.len(), hits.join("\n"));
}
