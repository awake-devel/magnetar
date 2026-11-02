//! False positives on misplaced windows: 64 KiB read at random offsets inside large files (as
//! if a probe had the wrong offset or a file started mid-piece) must not produce a confident
//! wrong type. MPEG-TS windows are legitimately MPEG-TS.
//!
//! usage: cargo run --release -p sniff --example fp -- <list-of-files> [windows-per-file]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use sniff::{Detector, SparseFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(list) = args.first() else {
        eprintln!("usage: fp <list-of-files> [windows-per-file]");
        std::process::exit(2);
    };
    let per: usize = args.get(1).and_then(|x| x.parse().ok()).unwrap_or(20);
    let text = std::fs::read_to_string(list).unwrap_or_default();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let (mut n, mut confident) = (0usize, 0usize);
    for path in text.lines().filter(|l| !l.is_empty()) {
        let Ok(mut f) = File::open(path) else {
            continue;
        };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        if size < 300_000 {
            continue;
        }
        for _ in 0..per {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let off = 65536 + seed % (size - 200_000);
            let mut b = Vec::new();
            if f.seek(SeekFrom::Start(off)).is_err()
                || (&mut f).take(65536).read_to_end(&mut b).is_err()
            {
                continue;
            }
            let mut s = SparseFile::new(size - off);
            s.insert(0, b);
            let d = Detector::new().analyze(&s).detection;
            n += 1;
            if d.confidence >= 80
                && !matches!(d.label.as_str(), "video.ts" | "video.m2ts" | "misc.zeros")
            {
                confident += 1;
                println!("{path}\t{off}\t{}\t{}", d.label, d.confidence);
            }
        }
    }
    println!(
        "windows {n}, confident (>=80) non-TS labels {confident}, panics {}",
        sniff::panics_caught()
    );
}
