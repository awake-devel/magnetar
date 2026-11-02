//! Orchestration: run every Rust check and signature rule, rank the candidates, decide which
//! ranges to ask for, raise the name flags, hand video containers to [`crate::video`], and the
//! fetch driver.

use std::borrow::Cow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::formats::{self, console, text};
use crate::label;
use crate::magic::{RuleResult, Signatures, eval_rule};
use crate::name;
use crate::sparse::{Range, SparseFile};
use crate::types::{Candidate, Caps, Detection, Flags, Hit, Outcome};
use crate::util::{entropy, ext, pre};
use crate::video;
use crate::view::View;

pub(crate) static PANICS: AtomicU64 = AtomicU64::new(0);

/// Parser panics caught since start (each one discards that parser's candidate). Must stay 0:
/// the property tests assert it.
pub fn panics_caught() -> u64 {
    PANICS.load(Ordering::Relaxed)
}

/// Ranges closer than this are fetched as one.
const MERGE_GAP: u64 = 512;

/// A detector: signature set plus caps.
#[derive(Clone, Debug)]
pub struct Detector<'s> {
    signatures: &'s Signatures,
    caps: Caps,
}

impl Default for Detector<'static> {
    fn default() -> Self {
        Detector {
            signatures: Signatures::builtin(),
            caps: Caps::default(),
        }
    }
}

/// Everything one pass found: the best-effort verdict and the ranges it would like next.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub detection: Detection,
    /// Missing ranges the winning parser (or the fallback) wants; empty = final.
    pub need: Vec<Range>,
}

/// What the fetch driver did.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub outcome: Outcome,
    /// The last pass, including what it still wanted when the budget ran out.
    pub analysis: Analysis,
    /// Follow-up rounds used (the first read is round 0).
    pub rounds: u32,
    pub bytes_fetched: u64,
    /// Every range fetched, in order.
    pub fetched: Vec<Range>,
    /// Why the rounds ended.
    pub stop: Stop,
}

/// Why the fetch driver stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// Nothing more was wanted.
    Done,
    /// `caps.rounds` follow-up rounds were used.
    Rounds,
    /// `caps.max_bytes` was spent.
    Budget,
    /// The fetch returned nothing new (short reads, EOF).
    NoProgress,
}

impl Detector<'static> {
    /// Built-in signatures, default caps.
    pub fn new() -> Detector<'static> {
        Detector::default()
    }
}

impl<'s> Detector<'s> {
    pub fn with_signatures(signatures: &'s Signatures) -> Detector<'s> {
        Detector {
            signatures,
            caps: Caps::default(),
        }
    }

    pub fn caps(mut self, caps: Caps) -> Detector<'s> {
        self.caps = caps;
        self
    }

    pub fn get_caps(&self) -> &Caps {
        &self.caps
    }

    /// One pass over the known bytes: `NeedMore` when the winner wants more ranges.
    pub fn detect(&self, file: &SparseFile) -> Outcome {
        outcome(self.analyze(file), false)
    }

    /// One pass that never asks for more: the best verdict from what is known (flagged
    /// `incomplete` when the winner wanted more).
    pub fn finish(&self, file: &SparseFile) -> Outcome {
        outcome(self.analyze(file), true)
    }

    /// The full pass: best-effort detection plus wanted ranges.
    pub fn analyze(&self, file: &SparseFile) -> Analysis {
        self.analyze_depth(file, 0)
    }

    fn analyze_depth(&self, file: &SparseFile, depth: u8) -> Analysis {
        let v = View::new(file, &self.caps, depth, self.signatures);
        let (mut hits, wants) = collect(&v, self.signatures);
        let e = ext(v.name());
        hits.sort_by(|a, b| {
            b.conf
                .cmp(&a.conf)
                .then_with(|| name::ext_matches(&b.label, &e).cmp(&name::ext_matches(&a.label, &e)))
                .then_with(|| b.strength.cmp(&a.strength))
        });
        let candidates: Vec<Candidate> = hits
            .iter()
            .take(16)
            .map(|h| Candidate {
                label: h.label.to_string(),
                confidence: h.conf,
                source: h.source.to_string(),
            })
            .collect();
        let mut it = hits.into_iter();
        let mut best = it.next().unwrap_or_else(|| fallback(&v));
        let mut need = missing(file, &best.need, &self.caps);
        let head_len = v.head().len() as u64;
        // Second chance for unidentified data: signatures at fixed offsets past a short head
        // (NCZ 0x4000, SNES 0x7FC0 / 0xFFC0, ISO/UDF 0x8000, XDVDFS 0x10000) or in the last
        // bytes (NRG, DMG, VHD, TGA).
        if (best.conf < 60 || best.label == "misc.zeros")
            && need.is_empty()
            && file.len() > head_len
        {
            let mut w = wants;
            // 0x7FC0..0x10800: also the SNES LoROM / HiROM internal headers (0x7FC0, 0xFFC0).
            w.push(Range::new(0x7FC0, 0x8840));
            w.push(Range::new(file.len().saturating_sub(512), 512));
            need = missing(file, &w, &self.caps);
        }
        let mut recon = None;
        if need.is_empty() && video::is_video_container(&best.label) {
            let r = video::analyze(file, &self.caps);
            need = missing(file, &r.need, &self.caps);
            // The recon read the bytes behind every track field: they inherit its flag.
            best.unverified |= r.unverified;
            if let Some(info) = &r.info {
                video_refines(&mut best, info, need.is_empty());
            }
            recon = r.info;
        }
        let mut detection = build(&best, &v, candidates);
        detection.video = recon;
        Analysis { detection, need }
    }

    /// Runs up to `caps.rounds` follow-up rounds through `fetch` (which returns the bytes of
    /// the range asked, or fewer at EOF). Fetched bytes are treated as verified; drive
    /// [`Detector::analyze`] yourself with [`SparseFile::insert_unverified`] otherwise.
    pub fn detect_with<F, E>(&self, mut fetch: F, len: u64, name: Option<&str>) -> Result<Report, E>
    where
        F: FnMut(Range) -> Result<Vec<u8>, E>,
    {
        let caps = &self.caps;
        let mut file = SparseFile::new(len);
        file.set_name(name.map(str::to_owned));
        let mut budget = caps.max_bytes;
        let mut fetched = Vec::new();
        let mut bytes_fetched = 0u64;
        let mut get = |file: &mut SparseFile, r: Range, budget: &mut u64| -> Result<bool, E> {
            let r = Range::new(r.offset, r.len.min(caps.max_range).min(*budget));
            let Some(r) = r.clamp(len) else {
                return Ok(false);
            };
            let mut b = fetch(r)?;
            // A fetch that answers more than was asked must not get past the budget or plant
            // bytes at offsets nobody asked for: keep the requested range only.
            b.truncate(usize::try_from(r.len).unwrap_or(usize::MAX));
            let n = b.len() as u64;
            *budget = budget.saturating_sub(n);
            bytes_fetched += n;
            fetched.push(r);
            file.insert(r.offset, b);
            Ok(n > 0)
        };
        let head = caps.head.min(len);
        get(&mut file, Range::new(0, head), &mut budget)?;
        if caps.tail > 0 && len > head {
            let t = caps.tail.min(len - head);
            get(&mut file, Range::new(len - t, t), &mut budget)?;
        }
        let mut rounds = 0u32;
        let mut stop = Stop::Done;
        let analysis = loop {
            let a = self.analyze(&file);
            if !a.need.is_empty() && rounds >= caps.rounds {
                stop = Stop::Rounds;
            } else if !a.need.is_empty() && budget == 0 {
                stop = Stop::Budget;
            }
            if a.need.is_empty() || stop != Stop::Done {
                break a;
            }
            rounds += 1;
            let mut progressed = false;
            for r in a.need.iter().take(caps.max_ranges) {
                for m in file.missing(*r) {
                    if budget == 0 {
                        break;
                    }
                    progressed |= get(&mut file, m, &mut budget)?;
                }
            }
            if !progressed {
                let a = self.analyze(&file);
                if !a.need.is_empty() {
                    stop = if budget == 0 {
                        Stop::Budget
                    } else {
                        Stop::NoProgress
                    };
                }
                break a;
            }
        };
        let mut analysis = analysis;
        analysis.detection.flags.incomplete = !analysis.need.is_empty();
        Ok(Report {
            outcome: outcome(analysis.clone(), true),
            analysis,
            rounds,
            bytes_fetched,
            fetched,
            stop,
        })
    }
}

fn outcome(a: Analysis, last: bool) -> Outcome {
    if !a.need.is_empty() && !last {
        return Outcome::NeedMore(a.need);
    }
    if a.detection.label == "misc.unknown" {
        return Outcome::Unknown;
    }
    let mut d = a.detection;
    d.flags.incomplete = !a.need.is_empty();
    Outcome::Detected(d)
}

/// One pass with the built-in signatures and default caps.
pub fn detect(file: &SparseFile) -> Outcome {
    Detector::new().detect(file)
}

/// Fetch driver with the built-in signatures (see [`Detector::detect_with`]).
pub fn detect_with<F, E>(fetch: F, len: u64, name: Option<&str>, caps: &Caps) -> Result<Report, E>
where
    F: FnMut(Range) -> Result<Vec<u8>, E>,
{
    Detector::new()
        .caps(caps.clone())
        .detect_with(fetch, len, name)
}

/// Label (and tar first entry) of decompressed bytes, for the gzip/zstd peeks.
/// Uses the caller's signature set, so a user rule labels a payload the same way compressed or
/// not.
pub(crate) fn inner_label(
    bytes: Vec<u8>,
    caps: &Caps,
    depth: u8,
    signatures: &Signatures,
) -> (String, Option<String>) {
    let mut f = SparseFile::new(u64::MAX / 2);
    f.insert(0, bytes);
    let a = Detector::with_signatures(signatures)
        .caps(caps.clone())
        .analyze_depth(&f, depth);
    let first = a.detection.field_str("first_entry").map(str::to_owned);
    (a.detection.label, first)
}

fn guarded(check: formats::Check, v: &View) -> Option<Hit> {
    match catch_unwind(AssertUnwindSafe(|| check(v))) {
        Ok(h) => h,
        Err(_) => {
            PANICS.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// Every candidate plus the ranges undecided signature rules would read.
fn collect(v: &View, sigs: &Signatures) -> (Vec<Hit>, Vec<Range>) {
    let mut hits = Vec::with_capacity(4);
    let mut wants = Vec::new();
    for (name, check) in formats::checks() {
        v.take_unverified();
        if let Some(mut h) = guarded(*check, v) {
            if h.source.is_empty() {
                h.source = Cow::Borrowed(name);
            }
            h.unverified = v.take_unverified();
            hits.push(h);
        }
    }
    for rule in sigs.rules() {
        v.take_unverified();
        match catch_unwind(AssertUnwindSafe(|| eval_rule(v, rule))) {
            Ok(RuleResult::Matched(mut h)) => {
                h.unverified = v.take_unverified();
                hits.push(*h);
            }
            Ok(RuleResult::Missing(r)) if rule.fetch => {
                wants.push(Range::new(r.offset, r.len.max(rule_len(rule, r))))
            }
            Ok(_) => {}
            Err(_) => {
                PANICS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if hits.iter().all(|h| h.conf < 60) {
        v.take_unverified();
        if let Some(mut h) = catch_unwind(AssertUnwindSafe(|| text::check(v)))
            .ok()
            .flatten()
        {
            h.source = Cow::Borrowed("text");
            h.unverified = v.take_unverified();
            hits.push(h);
        }
    }
    if hits.is_empty() {
        hits.push(fallback(v));
    }
    (hits, wants)
}

fn rule_len(rule: &crate::magic::Rule, r: Range) -> u64 {
    // A head rule wants everything up to its last fixed byte.
    if r.offset < rule.head {
        rule.head - r.offset
    } else {
        r.len
    }
}

fn fallback(v: &View) -> Hit {
    let h = v.head();
    let mut hit = if v.size() == 0 {
        Hit::new("misc.empty", 50)
    } else if !h.is_empty() && h.iter().all(|&b| b == 0) {
        // Only a whole file of zeros is a padding file; a zero head says little about the rest
        // (a HiROM SNES image starts with 32 KiB of zeros).
        if h.len() as u64 >= v.size() {
            Hit::new("misc.zeros", 70).d("note", "all-zero (padding file, sparse or preallocated)")
        } else {
            Hit::new("misc.zeros", 50).d("note", "the bytes read are all zero")
        }
    } else {
        Hit::new("misc.unknown", 10).d("entropy", format!("{:.2}", entropy(pre(h, 65536))))
    };
    hit.source = Cow::Borrowed("fallback");
    hit
}

/// The parts of `want` not known yet, clamped to the file and to `max_range`, sorted, merged.
fn missing(file: &SparseFile, want: &[Range], caps: &Caps) -> Vec<Range> {
    let mut out: Vec<Range> = want
        .iter()
        .flat_map(|r| file.missing(Range::new(r.offset, r.len.min(caps.max_range))))
        .collect();
    out.sort();
    let mut merged: Vec<Range> = Vec::with_capacity(out.len());
    for r in out {
        match merged.last_mut() {
            Some(l)
                if r.offset <= l.end().saturating_add(MERGE_GAP)
                    && l.end().max(r.end()) - l.offset <= caps.max_range =>
            {
                l.len = l.end().max(r.end()) - l.offset;
            }
            _ => merged.push(r),
        }
    }
    merged.retain(|r| r.len > 0);
    cap_ranges(&mut merged, caps);
    merged
}

/// Holds a list of wanted ranges to the caps: at most `max_ranges` ranges, none longer than
/// `max_range`, and no more than `max_bytes` in total (the last range is cut short), so a
/// caller that follows `NeedMore` itself never fetches more in one round than the driver
/// would.
pub(crate) fn cap_ranges(ranges: &mut Vec<Range>, caps: &Caps) {
    ranges.truncate(caps.max_ranges.max(1));
    let mut left = caps.max_bytes;
    ranges.retain_mut(|r| {
        r.len = r.len.min(caps.max_range.max(1)).min(left);
        left -= r.len;
        r.len > 0
    });
}

/// What a finished video recon adds to the type check's hit: an MP4 with sound tracks and no
/// picture is an `.m4a`, and a walk that found the tracks is the structural check (97).
fn video_refines(best: &mut Hit, info: &video::VideoInfo, complete: bool) {
    let tracks = info.video.len() + info.audio.len() + info.subtitles.len();
    if tracks == 0 {
        return;
    }
    if info.video.is_empty() && !info.audio.is_empty() && best.label() == "video.mp4" {
        best.relabel("audio.m4a");
    }
    if complete && best.conf < 97 && matches!(info.container.as_str(), "mp4" | "mov") {
        best.conf = 97;
    }
}

fn build(best: &Hit, v: &View, candidates: Vec<Candidate>) -> Detection {
    let id = best.label.to_string();
    let info = label::info(&id);
    let mime = best
        .mime
        .clone()
        .or_else(|| info.map(|i| i.mime.to_owned()))
        .unwrap_or_else(|| "application/octet-stream".into());
    let description = best
        .description
        .clone()
        .or_else(|| info.map(|i| i.description.to_owned()))
        .unwrap_or_else(|| id.clone());
    let (extension_mismatch, suspicious_executable) =
        name::flags(v.name(), &id, best.has("executables_inside"));
    Detection {
        game: best
            .game
            .clone()
            .or_else(|| console::game_from_label(&id, &best.fields)),
        video: None,
        flags: Flags {
            extension_mismatch,
            suspicious_executable,
            unverified: best.unverified,
            incomplete: false,
        },
        extensions: label::extensions(&id)
            .iter()
            .filter(|e| !e.is_empty())
            .map(|e| (*e).to_owned())
            .collect(),
        fields: best.fields.clone(),
        confidence: best.conf,
        label: id,
        mime,
        description,
        candidates,
    }
}
