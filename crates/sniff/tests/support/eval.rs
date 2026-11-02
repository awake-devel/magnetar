//! Corpus evaluation shared by `tests/corpus_eval.rs` and `examples/eval.rs`.
//!
//! A manifest is TSV: `path<TAB>label<TAB>origin`, with labels in the prototype's taxonomy
//! (`video/matroska`); relative paths resolve against the manifest's directory. Each file is
//! read only through the ranges the driver asks for.

#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sniff::{Caps, Outcome, Range, label};

/// Scratchpad corpora of the prototype (present only on the machine that built them).
pub const CORPUS_DIR: &str = "/tmp/claude-1000/-home-dude-magnetar/ab655fe2-024e-43d8-9df4-03634ee729f5/scratchpad/probes/filetype/corpus";

pub fn manifest(name: &str) -> Option<PathBuf> {
    let p = Path::new(CORPUS_DIR).join(name);
    p.is_file().then_some(p)
}

pub struct Row {
    pub path: String,
    pub want: String,
    pub got: String,
    pub confidence: u8,
    pub rounds: u32,
    pub bytes: u64,
    pub flags: String,
}

pub struct Score {
    pub rows: Vec<Row>,
    pub missing: usize,
}

impl Score {
    pub fn exact(&self) -> usize {
        self.rows.iter().filter(|r| r.want == r.got).count()
    }
    pub fn misses(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.want != r.got)
    }
    pub fn pct(&self) -> f64 {
        if self.rows.is_empty() {
            0.0
        } else {
            100.0 * self.exact() as f64 / self.rows.len() as f64
        }
    }
    pub fn median_bytes(&self) -> u64 {
        let mut b: Vec<u64> = self.rows.iter().map(|r| r.bytes).collect();
        b.sort_unstable();
        b.get(b.len() / 2).copied().unwrap_or(0)
    }
    pub fn p90_bytes(&self) -> u64 {
        let mut b: Vec<u64> = self.rows.iter().map(|r| r.bytes).collect();
        b.sort_unstable();
        b.get(b.len() * 9 / 10).copied().unwrap_or(0)
    }
}

fn read_range(f: &mut File, r: Range) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(r.offset))?;
    let mut v = Vec::new();
    f.take(r.len).read_to_end(&mut v)?;
    Ok(v)
}

/// Scores every file of `manifest` with the driver and `caps`.
pub fn run(manifest: &Path, caps: &Caps) -> Score {
    let root = manifest.parent().unwrap_or(Path::new("."));
    let text = std::fs::read_to_string(manifest).unwrap_or_default();
    let mut rows = Vec::new();
    let mut missing = 0;
    for line in text.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        let (Some(path), Some(legacy)) = (cols.first(), cols.get(1)) else {
            continue;
        };
        let full = if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            root.join(path)
        };
        let Ok(mut f) = File::open(&full) else {
            missing += 1;
            continue;
        };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let name = full
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let want =
            label::from_legacy(legacy).map_or_else(|| format!("?{legacy}"), |l| l.id.to_owned());
        let report =
            sniff::detect_with(|r| read_range(&mut f, r), size, Some(&name), caps).expect("read");
        let (got, confidence, flags) = match &report.outcome {
            Outcome::Detected(d) => {
                let mut fl = Vec::new();
                if d.flags.extension_mismatch {
                    fl.push("mismatch");
                }
                if d.flags.suspicious_executable {
                    fl.push("SUSPICIOUS");
                }
                if d.flags.incomplete {
                    fl.push("incomplete");
                }
                (d.label.clone(), d.confidence, fl.join("+"))
            }
            Outcome::Unknown => ("misc.unknown".to_owned(), 0, String::new()),
            Outcome::NeedMore(_) => ("needmore".to_owned(), 0, String::new()),
        };
        rows.push(Row {
            path: (*path).to_owned(),
            want,
            got,
            confidence,
            rounds: report.rounds,
            bytes: report.bytes_fetched,
            flags,
        });
    }
    Score { rows, missing }
}

/// The prototype's evaluation setting: 64 KiB head + 64 KiB tail, up to 6 rounds.
pub fn proto_caps() -> Caps {
    Caps {
        head: 65536,
        tail: 65536,
        rounds: 6,
        ..Caps::default()
    }
}
