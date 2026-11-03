//! Which files a header probe reads, and their first ranges (D117; port of the planner report's
//! `planner.py`, with the corrections of check0).
//!
//! Rules:
//! - the largest file is always a target, whatever its extension (the payload is always
//!   sniffed); extensions only pick the first ranges, never the verdict;
//! - skipped: junk (`.nfo`, `.txt`, `.sfv`, ...), samples under 20 % of the largest file, RAR
//!   volumes after the first, empty files (padding files are not display files);
//! - packs are grouped by directory template (digits → `#`) plus extension: video and other
//!   groups keep the largest and the median member, audio and image groups the largest;
//! - disc layouts from [`sniff::listing::classify`]: BDMV `.mpls`, `.clpi` and `index.bdmv`,
//!   VIDEO_TS `.ifo`, read whole (at most [`DISC_META_MAX`] each); all of them count as one target;
//! - at most `max_targets` other targets, by priority then size.
//!
//! First read: 64 KiB for video (TS too: the PMT gives every codec and language, check0 #4),
//! 16 KiB for everything else. Follow-ups (MP4 `moov`, ZIP central directory, PS5 CNT block, TS
//! audio) come from sniff's `NeedMore`, within the per-file [`sniff::Caps`].

use std::collections::HashMap;

use sniff::Range;

use super::msg::{BLOCK, Block};

/// First read of a video file.
pub const VIDEO_HEAD: u64 = 64 * 1024;
/// First read of any other file.
pub const HEAD: u64 = 16 * 1024;
/// Largest disc metadata file (`.mpls`, `.clpi`, `.ifo`, `index.bdmv`) read whole.
pub const DISC_META_MAX: u64 = 256 * 1024;
/// Priority of the payload, the largest file of the torrent (always a target).
pub const PAYLOAD: u8 = 12;
/// Most disc metadata files read per torrent (they count as one target together).
pub const DISC_META_FILES: usize = 40;

const VIDEO: &[&str] = &[
    "mkv", "webm", "mp4", "m4v", "mov", "avi", "wmv", "asf", "ts", "m2ts", "mts", "mpg", "mpeg",
    "vob", "flv", "ogv", "rmvb", "rm", "divx", "3gp", "evo", "m2v", "y4m",
];
const AUDIO: &[&str] = &[
    "flac", "mp3", "m4a", "m4b", "aac", "ogg", "opus", "wav", "ape", "wv", "dsf", "dff", "alac",
    "ac3", "dts", "mka", "aiff", "aif", "wma", "tak", "tta", "mpc", "eac3", "thd",
];
const IMG: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "avif", "psd", "raw", "cr2",
    "nef", "arw", "dng",
];
const JUNK: &[&str] = &[
    "nfo", "txt", "url", "html", "htm", "md5", "sfv", "sha1", "sha256", "par2", "torrent", "db",
    "ini", "log", "lnk", "xml", "json", "md", "diz", "website", "!qb", "part", "ds_store", "srr",
    "m3u", "m3u8", "cue",
];

/// How a file was picked (orders targets and picks the first read).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Video,
    Audio,
    Image,
    DiscMeta,
    Other,
}

/// One file to sniff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Display index (as [`crate::metainfo::InfoSummary::files`]).
    pub file: usize,
    pub kind: Kind,
    /// First ranges, file-relative.
    pub initial: Vec<Range>,
    /// Higher first: 12 the largest file, 11 disc metadata, then by kind.
    pub priority: u8,
}

/// A display file with its place in the piece space.
#[derive(Clone, Copy, Debug)]
pub struct PlanFile<'a> {
    pub path: &'a str,
    pub size: u64,
}

fn ext(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn base_lower(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

/// Digits → `#`, so `S01E01` … `S01E10` share a template.
fn template(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut digit = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !digit {
                out.push('#');
            }
            digit = true;
        } else {
            out.push(c);
            digit = false;
        }
    }
    out
}

/// `*.r00`, `*.001`, `*.part2.rar` and later: not the first volume of a RAR set.
fn later_rar_volume(lower: &str) -> bool {
    let e = ext(lower);
    if e.len() == 3 && e.starts_with('r') && e[1..].bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if e.len() == 3 && e.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if e == "rar"
        && let Some(stem) = lower.strip_suffix(".rar")
        && let Some(i) = stem.rfind(".part")
    {
        let n = &stem[i + 5..];
        return !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.trim_start_matches('0') != "1";
    }
    false
}

fn kind_of(e: &str) -> Kind {
    if VIDEO.contains(&e) {
        Kind::Video
    } else if AUDIO.contains(&e) {
        Kind::Audio
    } else if IMG.contains(&e) {
        Kind::Image
    } else {
        Kind::Other
    }
}

fn priority(k: Kind) -> u8 {
    match k {
        Kind::Video => 10,
        Kind::DiscMeta => 11,
        Kind::Other => 7,
        Kind::Audio => 6,
        Kind::Image => 2,
    }
}

struct Cand {
    file: usize,
    kind: Kind,
    size: u64,
    head: u64,
    pri: u8,
    group: String,
}

/// The targets of a torrent, best first (at most `max_targets` plus the disc metadata files).
pub fn plan(files: &[PlanFile<'_>], max_targets: usize) -> Vec<Target> {
    let biggest = files.iter().map(|f| f.size).max().unwrap_or(0);
    let Some(largest) = files.iter().position(|f| f.size == biggest && f.size > 0) else {
        return Vec::new();
    };
    let listings = sniff::listing::classify(files.iter().map(|f| (f.path, f.size)));
    let root_of = |label: &str| -> Option<String> {
        listings
            .iter()
            .find(|l| l.label == label)
            .map(|l| l.root.replace('\\', "/").to_ascii_lowercase())
    };
    let bdmv = root_of("disc.bluray.folder");
    let vts = root_of("disc.dvd_video.folder");
    let under = |root: &Option<String>, lower: &str| {
        root.as_ref()
            .is_some_and(|r| r.is_empty() || lower.starts_with(&format!("{r}/")))
    };
    let mut cands = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if f.size == 0 {
            continue;
        }
        let lower = f.path.replace('\\', "/").to_ascii_lowercase();
        let e = ext(&lower);
        let name = base_lower(&lower);
        let is_largest = i == largest;
        if !is_largest {
            if (name.contains("sample") || lower.contains("/sample/")) && f.size < biggest / 5 {
                continue;
            }
            if later_rar_volume(&lower) {
                continue;
            }
        }
        let mut kind = kind_of(&e);
        let mut head = if kind == Kind::Video {
            VIDEO_HEAD
        } else {
            HEAD
        };
        let disc_meta = (under(&bdmv, &lower)
            && (e == "mpls" || e == "clpi" || name == "index.bdmv"))
            || (under(&vts, &lower) && e == "ifo");
        if disc_meta && !is_largest {
            if f.size > DISC_META_MAX {
                continue;
            }
            kind = Kind::DiscMeta;
            head = f.size;
        } else if !is_largest && JUNK.contains(&e.as_str()) {
            continue;
        }
        let mut pri = priority(kind);
        if is_largest {
            pri = PAYLOAD;
        }
        let dir = lower.rsplit_once('/').map_or("", |(d, _)| d);
        cands.push(Cand {
            file: i,
            kind,
            size: f.size,
            head: head.min(f.size),
            pri,
            group: format!("{}|{e}", template(dir)),
        });
    }
    // Packs: per directory template and extension, the largest and the median member (one
    // for audio and images); disc metadata is kept whole.
    let mut groups: HashMap<String, Vec<Cand>> = HashMap::new();
    for c in cands {
        groups.entry(c.group.clone()).or_default().push(c);
    }
    let mut keep = Vec::new();
    for (_, mut g) in groups {
        if g.len() <= 2 || g[0].kind == Kind::DiscMeta {
            keep.extend(g);
            continue;
        }
        g.sort_by(|a, b| b.size.cmp(&a.size).then(a.file.cmp(&b.file)));
        let median = g.len() / 2;
        let one = matches!(g[0].kind, Kind::Audio | Kind::Image);
        let mut it = g.into_iter().enumerate();
        // The largest file of the torrent stays even when a group would drop it.
        for (i, c) in &mut it {
            if i == 0 || (!one && i == median) || c.pri == PAYLOAD {
                keep.push(c);
            }
        }
    }
    keep.sort_by(|a, b| {
        b.pri
            .cmp(&a.pri)
            .then(b.size.cmp(&a.size))
            .then(a.file.cmp(&b.file))
    });
    let mut out = Vec::new();
    let mut n = 0usize;
    let mut meta = 0usize;
    for c in keep {
        if c.kind == Kind::DiscMeta {
            if meta >= DISC_META_FILES {
                continue;
            }
            meta += 1;
        } else {
            if n >= max_targets.max(1) {
                continue;
            }
            n += 1;
        }
        out.push(Target {
            file: c.file,
            kind: c.kind,
            initial: vec![Range::new(0, c.head)],
            priority: c.pri,
        });
    }
    out
}

/// The 16 KiB request blocks that hold bytes `[abs, abs + len)` of a piece space of `total`
/// bytes cut in `piece_len` pieces (blocks on each piece's own 16 KiB grid; the last block of a
/// piece may be short). With `whole`, every block of every touched piece.
pub fn blocks_for(abs: u64, len: u64, piece_len: u64, total: u64, whole: bool) -> Vec<Block> {
    let mut out = Vec::new();
    if len == 0 || abs >= total || piece_len == 0 {
        return out;
    }
    let end = abs.saturating_add(len).min(total);
    let first = abs / piece_len;
    let last = (end - 1) / piece_len;
    for p in first..=last {
        let Ok(index) = u32::try_from(p) else { break };
        let pstart = p * piece_len;
        let plen = piece_len.min(total - pstart);
        let (lo, hi) = if whole {
            (0, plen)
        } else {
            (abs.max(pstart) - pstart, end.min(pstart + plen) - pstart)
        };
        let mut b = lo / u64::from(BLOCK) * u64::from(BLOCK);
        while b < hi {
            let blen = u64::from(BLOCK).min(plen - b);
            out.push(Block {
                index,
                begin: b as u32,
                len: blen as u32,
            });
            b += u64::from(BLOCK);
        }
    }
    out
}

/// Pieces that hold bytes `[abs, abs + len)`.
pub fn pieces_for(abs: u64, len: u64, piece_len: u64, total: u64) -> std::ops::RangeInclusive<u32> {
    if len == 0 || abs >= total || piece_len == 0 {
        #[allow(clippy::reversed_empty_ranges)]
        return 1..=0;
    }
    let end = abs.saturating_add(len).min(total);
    let first = u32::try_from(abs / piece_len).unwrap_or(u32::MAX);
    let last = u32::try_from((end - 1) / piece_len).unwrap_or(u32::MAX);
    first..=last
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn f(path: &str, size: u64) -> PlanFile<'_> {
        PlanFile { path, size }
    }

    fn files_of(t: &[Target]) -> Vec<usize> {
        t.iter().map(|t| t.file).collect()
    }

    #[test]
    fn single_video_and_junk() {
        let files = [
            f("Movie.2024.1080p.mkv", 4000 * MIB),
            f("Movie.nfo", 2000),
            f("Sample/movie.sample.mkv", 30 * MIB),
            f("Subs/eng.srt", 80_000),
        ];
        let t = plan(&files, 8);
        assert_eq!(files_of(&t), vec![0, 3]);
        assert_eq!(t[0].priority, 12);
        assert_eq!(t[0].initial, vec![Range::new(0, VIDEO_HEAD)]);
        assert_eq!(t[1].initial, vec![Range::new(0, HEAD)]);
    }

    #[test]
    fn the_largest_file_is_sniffed_whatever_its_name() {
        let files = [f("readme.txt", 10), f("payload.nfo", 900 * MIB)];
        assert_eq!(files_of(&plan(&files, 8)), vec![1]);
        assert_eq!(plan(&files, 8)[0].initial, vec![Range::new(0, HEAD)]);
        assert!(plan(&[f("a", 0)], 8).is_empty());
    }

    #[test]
    fn packs_keep_largest_and_median() {
        let names: Vec<String> = (1..=10).map(|i| format!("Show/S01E{i:02}.mkv")).collect();
        let files: Vec<PlanFile> = names
            .iter()
            .enumerate()
            .map(|(i, n)| f(n, (500 + i as u64) * MIB))
            .collect();
        let t = plan(&files, 8);
        // Largest is E10 (index 9); sorted by size desc the median is index 5 → E05 (index 4).
        assert_eq!(files_of(&t), vec![9, 4]);
        let tracks: Vec<String> = (1..=12)
            .map(|i| format!("Album/{i:02} Track.flac"))
            .collect();
        let mut files: Vec<PlanFile> = tracks.iter().map(|n| f(n, 30 * MIB)).collect();
        files.push(f("Album/cover.jpg", 2 * MIB));
        let t = plan(&files, 8);
        assert_eq!(t.len(), 2, "one track and the cover: {t:?}");
    }

    #[test]
    fn rar_volumes_and_cap() {
        let mut names = vec!["x.rar".to_string()];
        names.extend((0..20).map(|i| format!("x.r{i:02}")));
        names.push("x.part2.rar".into());
        let files: Vec<PlanFile> = names.iter().map(|n| f(n, 100 * MIB)).collect();
        assert_eq!(files_of(&plan(&files, 8)), vec![0]);
        // Twenty directories that do not share a template: twenty groups, cut to the cap.
        let names: Vec<String> = (b'a'..=b't')
            .map(|c| format!("{}/game.iso", c as char))
            .collect();
        let files: Vec<PlanFile> = names
            .iter()
            .enumerate()
            .map(|(i, n)| f(n, (100 + i as u64) * MIB))
            .collect();
        let t = plan(&files, 8);
        assert_eq!(t.len(), 8);
        assert_eq!(t[0].file, 19);
        assert_eq!(plan(&files, 3).len(), 3);
        assert!(later_rar_volume("a.part02.rar"));
        assert!(!later_rar_volume("a.part01.rar"));
        assert!(!later_rar_volume("a.part1.rar"));
        assert!(later_rar_volume("a.001"));
    }

    #[test]
    fn bdmv_and_video_ts_metadata_is_one_target() {
        let files = [
            f("Movie/BDMV/index.bdmv", 280),
            f("Movie/BDMV/MovieObject.bdmv", 300),
            f("Movie/BDMV/PLAYLIST/00000.mpls", 400),
            f("Movie/BDMV/PLAYLIST/00001.mpls", 900),
            f("Movie/BDMV/CLIPINF/00000.clpi", 700),
            f("Movie/BDMV/STREAM/00000.m2ts", 30_000 * MIB),
            f("Movie/BDMV/STREAM/00001.m2ts", 200 * MIB),
            f("Movie/CERTIFICATE/id.bdmv", 100),
        ];
        let t = plan(&files, 1);
        // The M2TS payload (cap 1) and all four metadata files.
        assert_eq!(t[0].file, 5);
        let mut meta: Vec<usize> = t
            .iter()
            .filter(|t| t.kind == Kind::DiscMeta)
            .map(|t| t.file)
            .collect();
        meta.sort();
        assert_eq!(meta, vec![0, 2, 3, 4]);
        assert!(
            t.iter()
                .filter(|t| t.kind == Kind::DiscMeta)
                .all(|t| t.initial[0].len == files[t.file].size)
        );
        let dvd = [
            f("DVD/VIDEO_TS/VIDEO_TS.IFO", 12_000),
            f("DVD/VIDEO_TS/VIDEO_TS.BUP", 12_000),
            f("DVD/VIDEO_TS/VTS_01_0.IFO", 40_000),
            f("DVD/VIDEO_TS/VTS_01_1.VOB", 1000 * MIB),
            f("DVD/VIDEO_TS/VTS_01_2.VOB", 900 * MIB),
        ];
        let t = plan(&dvd, 8);
        assert_eq!(t[0].file, 3);
        let meta: Vec<usize> = t
            .iter()
            .filter(|t| t.kind == Kind::DiscMeta)
            .map(|t| t.file)
            .collect();
        assert_eq!(meta.len(), 2, "{t:?}");
    }

    #[test]
    fn block_mapping() {
        let pl = 64 * 1024;
        let total = 10 * pl + 100;
        // 100 bytes at 20 000: block 16 KiB of piece 0.
        let b = blocks_for(20_000, 100, pl, total, false);
        assert_eq!(
            b,
            vec![Block {
                index: 0,
                begin: 16384,
                len: 16384
            }]
        );
        // Whole piece: 4 blocks.
        assert_eq!(blocks_for(20_000, 100, pl, total, true).len(), 4);
        // Across a piece boundary.
        let b = blocks_for(pl - 10, 20, pl, total, false);
        assert_eq!(
            (b[0].index, b[0].begin, b[1].index, b[1].begin),
            (0, 49152, 1, 0)
        );
        // Last, short piece and the end of the space.
        let b = blocks_for(10 * pl, 5000, pl, total, true);
        assert_eq!(
            b,
            vec![Block {
                index: 10,
                begin: 0,
                len: 100
            }]
        );
        assert!(blocks_for(total, 1, pl, total, false).is_empty());
        // Piece length not a multiple of 16 KiB: the last block of each piece is short.
        let b = blocks_for(0, 40_000, 20_000, 100_000, false);
        assert_eq!(
            b.iter()
                .map(|b| (b.index, b.begin, b.len))
                .collect::<Vec<_>>(),
            vec![
                (0, 0, 16384),
                (0, 16384, 3616),
                (1, 0, 16384),
                (1, 16384, 3616)
            ]
        );
        assert_eq!(pieces_for(pl - 1, 2, pl, total), 0..=1);
        assert!(pieces_for(total, 2, pl, total).is_empty());
    }
}
