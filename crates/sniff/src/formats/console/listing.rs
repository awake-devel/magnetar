//! Classification from a torrent file list (paths and sizes, no bytes): game folder dumps
//! (`sce_sys/param.json`, `PS3_GAME/PARAM.SFO`, `meta/meta.xml`, `default.xbe`, Games on
//! Demand), disc folders (`BDMV/`, `VIDEO_TS/`) and split files / multi-volume archives
//! (`*.part1.rar`, `*.r00`, `*.7z.001`, `*.z01`, `*_0.pkg`, `game.nsp/00`).
//!
//! The result names the files worth fetching ([`Listing::metadata`]: pass their bytes to
//! [`crate::detect`] to get the ids) and, for split files, the parts in order
//! ([`Listing::parts`]): a [`crate::SparseFile`] of [`Listing::total_size`] bytes maps its
//! offsets into the parts in that order, so a split PS5 package is read like a whole one.
//!
//! Path layouts come from the usual dump tools; UNVERIFIED against real torrents.

use std::collections::HashMap;

use serde::Serialize;

use crate::formats::console::{Platform, db_id_valid};

/// Most entries looked at (a torrent with more is classified on the first ones).
const MAX_ENTRIES: usize = 200_000;
const MAX_PATH: usize = 4096;
const MAX_RESULTS: usize = 64;
const MAX_PARTS: usize = 1024;

/// One structure found in the file list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Listing {
    /// `game.ps5.folder`, `game.ps4.folder`, `game.vita.folder`, `game.ps3.folder`,
    /// `game.psp.folder`, `game.wiiu.folder`, `game.wiiu.nus`, `game.xbox.folder`,
    /// `game.x360.folder`, `game.x360.god`, `game.pkg.split`, `game.switch.nsp.split`,
    /// `disc.bluray.folder`, `disc.dvd_video.folder`, `archive.multipart`.
    pub label: &'static str,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Same scale as [`crate::Detection::confidence`]: a layout with its marker file plus a
    /// companion is 90+, a lone marker 80+.
    pub confidence: u8,
    /// Directory holding the structure ("" = the torrent root), or the common stem of a split
    /// (case kept, e.g. `dir/Movie` for `dir/Movie.part1.rar`).
    pub root: String,
    /// A title ID found in the paths (`CUSA00207-app`, `BLES00680`, a GoD title ID).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_id: Option<String>,
    /// Kind of split for `archive.multipart`: `rar`, `rar-old`, `7z`, `zip`, `split`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<&'static str>,
    /// Files whose bytes give the ids (param.json, PARAM.SFO, meta.xml, default.xbe, ...).
    pub metadata: Vec<String>,
    /// Parts of a split file in order.
    pub parts: Vec<String>,
    /// Total size of the files counted for the structure (all parts for a split).
    pub total_size: u64,
}

struct F<'a> {
    path: &'a str,
    lower: String,
    size: u64,
}

/// Most marker files (param.json, PARAM.SFO, ...) and split groups evaluated: each costs at
/// most a few binary searches plus a scan of its own subtree, charged to [`SCAN_BUDGET`].
const MAX_MARKERS: usize = 4096;
/// Entries visited by subtree scans over one call (a hostile list of nested roots stops
/// here instead of costing quadratic time).
const SCAN_BUDGET: usize = 4_000_000;
/// Structures collected before ranking (the result keeps [`MAX_RESULTS`]).
const MAX_COLLECT: usize = 4096;

/// The file list sorted by lower-cased path: everything under a directory is one contiguous
/// slice (binary search), sizes sum through a prefix table, and every scan is charged to a
/// shared budget. No lookup rescans the whole list.
struct Idx<'a> {
    fs: Vec<F<'a>>,
    /// `sums[i]` = total size of `fs[..i]`.
    sums: Vec<u128>,
    budget: std::cell::Cell<usize>,
    out: Vec<Listing>,
    seen: std::collections::HashSet<(&'static str, String)>,
}

impl<'a> Idx<'a> {
    fn new(mut fs: Vec<F<'a>>) -> Idx<'a> {
        fs.sort_by(|a, b| a.lower.cmp(&b.lower));
        let mut sums = Vec::with_capacity(fs.len() + 1);
        let mut t = 0u128;
        sums.push(0);
        for f in &fs {
            t = t.saturating_add(u128::from(f.size));
            sums.push(t);
        }
        Idx {
            fs,
            sums,
            budget: std::cell::Cell::new(SCAN_BUDGET),
            out: Vec::new(),
            seen: std::collections::HashSet::new(),
        }
    }

    /// Index span of the entries whose lower-cased path starts with `prefix`.
    fn span(&self, prefix: &str) -> (usize, usize) {
        let lo = self.fs.partition_point(|f| f.lower.as_str() < prefix);
        let n = self
            .fs
            .get(lo..)
            .map_or(0, |r| r.partition_point(|f| f.lower.starts_with(prefix)));
        (lo, lo + n)
    }

    /// Entries under directory `root` ("" = everything).
    fn under(&self, root: &str) -> &[F<'a>] {
        let (lo, hi) = if root.is_empty() {
            (0, self.fs.len())
        } else {
            self.span(&format!("{root}/"))
        };
        self.fs.get(lo..hi).unwrap_or(&[])
    }

    /// Total size under `root`.
    fn total(&self, root: &str) -> u64 {
        let (lo, hi) = if root.is_empty() {
            (0, self.fs.len())
        } else {
            self.span(&format!("{root}/"))
        };
        let s = |i: usize| self.sums.get(i).copied().unwrap_or(0);
        u64::try_from(s(hi).saturating_sub(s(lo))).unwrap_or(u64::MAX)
    }

    /// `root/suffix` is in the list (exact, lower-cased).
    fn has(&self, root: &str, suffix: &str) -> bool {
        let full = if root.is_empty() {
            suffix.to_owned()
        } else {
            format!("{root}/{suffix}")
        };
        self.exists(&full)
    }

    fn exists(&self, lower: &str) -> bool {
        self.fs
            .binary_search_by(|f| f.lower.as_str().cmp(lower))
            .is_ok()
    }

    /// Some entry under `root` (relative path) satisfies `pred`; scans charge the budget and
    /// an exhausted budget answers false.
    fn any(&self, root: &str, pred: &dyn Fn(&str) -> bool) -> bool {
        self.under(root).iter().any(|g| {
            let left = self.budget.get();
            if left == 0 {
                return false;
            }
            self.budget.set(left - 1);
            pred(rel(g, root))
        })
    }

    /// Entries under `root` matching `pred`, at most `max`, budget-charged.
    fn filter(&self, root: &str, max: usize, pred: &dyn Fn(&str) -> bool) -> Vec<&F<'a>> {
        let mut v = Vec::new();
        for g in self.under(root) {
            let left = self.budget.get();
            if left == 0 || v.len() >= max {
                break;
            }
            self.budget.set(left - 1);
            if pred(rel(g, root)) {
                v.push(g);
            }
        }
        v
    }

    /// The root as spelled in the torrent (the matching is case-insensitive).
    fn original_root(&self, root: &str) -> String {
        if root.is_empty() {
            return String::new();
        }
        let (lo, hi) = self.span(root);
        self.fs
            .get(lo..hi)
            .and_then(|r| r.first())
            .and_then(|f| {
                f.path
                    .replace('\\', "/")
                    .trim_start_matches('/')
                    .get(..root.len())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| root.to_owned())
    }

    /// Records a structure once per (label, root); false when it was already there or the
    /// collection is full.
    fn add(&mut self, l: Listing, key_root: &str) -> bool {
        if self.out.len() >= MAX_COLLECT {
            return false;
        }
        if !self.seen.insert((l.label, key_root.to_owned())) {
            return false;
        }
        self.out.push(l);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        label: &'static str,
        description: &'static str,
        platform: Option<Platform>,
        confidence: u8,
        root: &str,
        metadata: Vec<String>,
    ) {
        if self.seen.contains(&(label, root.to_owned())) || self.out.len() >= MAX_COLLECT {
            return;
        }
        let title_id = metadata
            .iter()
            .find_map(|m| sony_id_in(m))
            .or_else(|| self.under(root).first().and_then(|f| sony_id_in(f.path)));
        let l = Listing {
            label,
            description,
            platform,
            confidence,
            root: self.original_root(root),
            title_id,
            split: None,
            metadata,
            parts: Vec::new(),
            total_size: self.total(root),
        };
        self.add(l, root);
    }
}

fn dir_of(p: &str) -> &str {
    p.rfind('/').map_or("", |i| p.get(..i).unwrap_or(""))
}

fn base_of(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// A Sony title ID in one of the path components (`CUSA00207-app`, `PPSA12544`).
fn sony_id_in(path: &str) -> Option<String> {
    for comp in path.split('/') {
        let b = comp.as_bytes();
        for start in 0..b.len().saturating_sub(8) {
            let Some(w) = comp.get(start..start + 9) else {
                continue;
            };
            let wb = w.as_bytes();
            let before_ok = start == 0 || !b.get(start - 1).is_some_and(u8::is_ascii_alphanumeric);
            let after_ok = !b.get(start + 9).is_some_and(u8::is_ascii_digit);
            if before_ok
                && after_ok
                && wb.iter().take(4).all(u8::is_ascii_uppercase)
                && wb.iter().skip(4).all(u8::is_ascii_digit)
                && db_id_valid(Platform::Ps3, w)
            {
                return Some(w.to_owned());
            }
        }
    }
    None
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Classifies a file list. Paths use `/` or `\`; order does not matter. Returns the
/// structures found, most confident first.
///
/// Cost is O(n log n) in the number of entries: lookups are binary searches over the sorted
/// list, and subtree scans share a fixed budget (a hostile list is classified on what the
/// budget covered).
pub fn classify<'a, I>(files: I) -> Vec<Listing>
where
    I: IntoIterator<Item = (&'a str, u64)>,
{
    let fs: Vec<F> = files
        .into_iter()
        .take(MAX_ENTRIES)
        .filter(|(p, _)| !p.is_empty() && p.len() <= MAX_PATH)
        .map(|(p, size)| F {
            path: p,
            lower: p
                .replace('\\', "/")
                .trim_start_matches('/')
                .to_ascii_lowercase(),
            size,
        })
        .collect();
    let mut idx = Idx::new(fs);
    folders(&mut idx);
    splits(&mut idx);
    let mut out = idx.out;
    out.sort_by(|a, b| b.confidence.cmp(&a.confidence).then(a.root.cmp(&b.root)));
    out.truncate(MAX_RESULTS);
    out
}

fn rel<'s>(f: &'s F, root: &str) -> &'s str {
    if root.is_empty() {
        &f.lower
    } else {
        f.lower.get(root.len() + 1..).unwrap_or("")
    }
}

fn folders(idx: &mut Idx) {
    // Marker hits first (owned, so the index can be borrowed mutably while pushing).
    let mut hits: Vec<(String, &'static str, String)> = Vec::new();
    for f in &idx.fs {
        if hits.len() >= MAX_MARKERS {
            break;
        }
        let l = f.lower.as_str();
        if let Some((stem, marker)) = MARKERS
            .iter()
            .find_map(|m| strip_tail(l, m).map(|s| (s, *m)))
        {
            hits.push((stem.to_owned(), marker, f.path.replace('\\', "/")));
        }
    }
    for (root, marker, path) in hits {
        let root = root.as_str();
        let meta = vec![path];
        match marker {
            "sce_sys/param.json" => {
                let conf = if idx.has(root, "eboot.bin") { 95 } else { 85 };
                idx.push(
                    "game.ps5.folder",
                    "PS5 game folder dump",
                    Some(Platform::Ps5),
                    conf,
                    root,
                    meta,
                );
            }
            "sce_sys/param.sfo" => {
                if idx.has(root, "sce_sys/param.json") {
                    continue;
                }
                let vita = idx.any(root, &|r| {
                    r.ends_with(".suprx") || r.starts_with("sce_sys/livearea/")
                });
                let ps4 = idx.any(root, &|r| {
                    r.ends_with(".prx")
                        || r.starts_with("sce_sys/trophy/")
                        || r == "sce_sys/npbind.dat"
                });
                let eboot = idx.has(root, "eboot.bin");
                let id = sony_id_in(root);
                let by_id = id
                    .as_deref()
                    .and_then(crate::formats::console::sony::sony_platform);
                let (label, desc, p) = match (vita, ps4, by_id) {
                    (_, _, Some(Platform::Ps4)) | (false, true, _) => (
                        "game.ps4.folder",
                        "PS4 game folder dump",
                        Some(Platform::Ps4),
                    ),
                    (_, _, Some(Platform::PsVita)) | (true, false, _) => (
                        "game.vita.folder",
                        "PS Vita game folder dump",
                        Some(Platform::PsVita),
                    ),
                    _ => ("game.ps4.folder", "PS4 or PS Vita game folder dump", None),
                };
                let conf = match (eboot, p.is_some()) {
                    (true, true) => 93,
                    (true, false) | (false, true) => 85,
                    _ => 80,
                };
                idx.push(label, desc, p, conf, root, meta);
            }
            "ps3_game/param.sfo" | "ps3_disc.sfb" => {
                let conf = if idx.has(root, "ps3_game/usrdir/eboot.bin")
                    || idx.has(root, "ps3_disc.sfb")
                {
                    95
                } else {
                    88
                };
                let sfo = if root.is_empty() {
                    "ps3_game/param.sfo".to_owned()
                } else {
                    format!("{root}/ps3_game/param.sfo")
                };
                let (lo, hi) = idx.span(&sfo);
                let meta = idx
                    .fs
                    .get(lo..hi)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|g| g.lower == sfo)
                    .take(4)
                    .map(|g| g.path.replace('\\', "/"))
                    .collect();
                idx.push(
                    "game.ps3.folder",
                    "PS3 game folder (JB / disc) dump",
                    Some(Platform::Ps3),
                    conf,
                    root,
                    meta,
                );
            }
            "psp_game/param.sfo" => {
                let conf = if idx.has(root, "psp_game/sysdir/eboot.bin") {
                    95
                } else {
                    88
                };
                idx.push(
                    "game.psp.folder",
                    "PSP UMD folder dump",
                    Some(Platform::Psp),
                    conf,
                    root,
                    meta,
                );
            }
            // Memory Stick layout: PSP/GAME/<name>/EBOOT.PBP (homebrew, PSN games, PS1
            // eboots); also seen zipped as <name>/EBOOT.PBP alone (Apollo).
            "eboot.pbp" => {
                if root.is_empty() {
                    continue;
                }
                let parent = dir_of(root);
                let ms = parent == "psp/game"
                    || parent.ends_with("/psp/game")
                    || base_of(parent).starts_with("game")
                        && dir_of(parent).rsplit('/').next() == Some("psp");
                idx.push(
                    "game.psp.folder",
                    "PSP Memory Stick game folder (PSP/GAME/<name>/EBOOT.PBP)",
                    Some(Platform::Psp),
                    if ms { 90 } else { 80 },
                    root,
                    meta,
                );
            }
            "meta/meta.xml" => {
                let code = idx.any(root, &|r| {
                    r.starts_with("code/") && (r.ends_with(".rpx") || r.ends_with("app.xml"))
                });
                if !code {
                    continue;
                }
                let conf = if idx.any(root, &|r| r.starts_with("content/")) {
                    95
                } else {
                    88
                };
                idx.push(
                    "game.wiiu.folder",
                    "Wii U game folder (loadiine / WUP) dump",
                    Some(Platform::WiiU),
                    conf,
                    root,
                    meta,
                );
            }
            "title.tmd" => {
                let apps = idx.any(root, &|r| {
                    r.strip_suffix(".app").is_some_and(|h| is_hex(h, 8))
                });
                if !apps {
                    continue;
                }
                let conf = if idx.has(root, "title.tik") { 90 } else { 82 };
                idx.push(
                    "game.wiiu.nus",
                    "Nintendo CDN (NUS) download: title.tmd + .app",
                    Some(Platform::WiiU),
                    conf,
                    root,
                    meta,
                );
            }
            "default.xbe" => {
                idx.push(
                    "game.xbox.folder",
                    "Xbox game folder dump",
                    Some(Platform::Xbox),
                    88,
                    root,
                    meta,
                );
            }
            "default.xex" => {
                idx.push(
                    "game.x360.folder",
                    "Xbox 360 game folder dump",
                    Some(Platform::Xbox360),
                    88,
                    root,
                    meta,
                );
            }
            "bdmv/index.bdmv" => {
                let streams = idx.any(root, &|r| {
                    r.starts_with("bdmv/stream/") && r.ends_with(".m2ts")
                });
                let conf = if streams { 96 } else { 88 };
                let mut meta = meta;
                meta.extend(
                    idx.filter(root, 4, &|r| {
                        r.starts_with("bdmv/meta/dl/bdmt_") && r.ends_with(".xml")
                    })
                    .into_iter()
                    .map(|g| g.path.replace('\\', "/")),
                );
                idx.push(
                    "disc.bluray.folder",
                    "Blu-ray (BDMV) folder",
                    None,
                    conf,
                    root,
                    meta,
                );
            }
            "video_ts/video_ts.ifo" => {
                let vobs = idx.any(root, &|r| r.starts_with("video_ts/") && r.ends_with(".vob"));
                idx.push(
                    "disc.dvd_video.folder",
                    "DVD-Video (VIDEO_TS) folder",
                    None,
                    if vobs { 96 } else { 88 },
                    root,
                    meta,
                );
            }
            _ => {}
        }
    }
    // Blu-ray / DVD folders without their index file: streams alone.
    let mut loose: Vec<(&'static str, &'static str, String)> = Vec::new();
    for f in &idx.fs {
        if loose.len() >= MAX_MARKERS {
            break;
        }
        let l = f.lower.as_str();
        if let Some(i) = l.find("bdmv/stream/").filter(|_| l.ends_with(".m2ts")) {
            let root = if i == 0 {
                ""
            } else {
                l.get(..i - 1).unwrap_or("")
            };
            loose.push((
                "disc.bluray.folder",
                "Blu-ray (BDMV) folder",
                root.to_owned(),
            ));
        }
        if let Some(i) = l.find("video_ts/").filter(|_| l.ends_with(".vob")) {
            let root = if i == 0 {
                ""
            } else {
                l.get(..i - 1).unwrap_or("")
            };
            loose.push((
                "disc.dvd_video.folder",
                "DVD-Video (VIDEO_TS) folder",
                root.to_owned(),
            ));
        }
        // Consecutive streams of one folder: skip the duplicates before they cost a push.
        if loose.len() >= 2 && loose.last() == loose.get(loose.len() - 2) {
            loose.pop();
        }
    }
    for (label, desc, root) in loose {
        idx.push(label, desc, None, 85, &root, Vec::new());
    }
    god(idx);
}

const MARKERS: &[&str] = &[
    "sce_sys/param.json",
    "sce_sys/param.sfo",
    "ps3_game/param.sfo",
    "ps3_disc.sfb",
    "psp_game/param.sfo",
    "eboot.pbp",
    "meta/meta.xml",
    "title.tmd",
    "default.xbe",
    "default.xex",
    "bdmv/index.bdmv",
    "video_ts/video_ts.ifo",
];

/// `l` == `tail` (root "") or ends with `/tail` (root = the prefix).
fn strip_tail<'s>(l: &'s str, tail: &str) -> Option<&'s str> {
    if l == tail {
        return Some("");
    }
    let s = l.strip_suffix(tail)?;
    s.strip_suffix('/')
}

/// Xbox 360 Games on Demand: `<title id>/00007000/<40 hex>` plus `<40 hex>.data/Data0000`.
fn god(idx: &mut Idx) {
    let mut found: Vec<(String, String, u8, String)> = Vec::new();
    for f in &idx.fs {
        if found.len() >= MAX_MARKERS {
            break;
        }
        let mut it = f.lower.rsplit('/');
        let (Some(hash), Some(ty), Some(tid)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if !(is_hex(tid, 8) && ty == "00007000" && is_hex(hash, 40)) {
            continue;
        }
        let data = format!("{}.data/data0000", f.lower);
        let conf = if idx.exists(&data) { 95 } else { 85 };
        let root = dir_of(dir_of(&f.lower)).to_owned();
        found.push((
            root,
            tid.to_ascii_uppercase(),
            conf,
            f.path.replace('\\', "/"),
        ));
    }
    for (root, tid, conf, path) in found {
        if idx.seen.contains(&("game.x360.god", root.clone())) {
            continue;
        }
        let (lo, hi) = idx.span(&root);
        let s = |i: usize| idx.sums.get(i).copied().unwrap_or(0);
        let total = u64::try_from(s(hi).saturating_sub(s(lo))).unwrap_or(u64::MAX);
        let l = Listing {
            label: "game.x360.god",
            description: "Xbox 360 Games on Demand (STFS header + data files)",
            platform: Some(Platform::Xbox360),
            confidence: conf,
            root: idx.original_root(&root),
            title_id: Some(tid),
            split: None,
            metadata: vec![path],
            parts: Vec::new(),
            total_size: total,
        };
        idx.add(l, &root);
    }
}

/// (group stem, split kind, part number) of a file that looks like one part of a split.
fn part_of(l: &str) -> Option<(String, &'static str, u32)> {
    let dir = dir_of(l);
    let base = base_of(l);
    let num = |s: &str| -> Option<u32> {
        (!s.is_empty() && s.len() <= 4 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let join = |stem: &str| {
        if dir.is_empty() {
            stem.to_owned()
        } else {
            format!("{dir}/{stem}")
        }
    };
    // name.part01.rar
    if let Some(s) = base.strip_suffix(".rar")
        && let Some((stem, n)) = s.rsplit_once(".part")
        && let Some(n) = num(n)
    {
        return Some((join(stem), "rar", n));
    }
    // name.rar + name.r00, name.r01, ...
    if let Some(stem) = base.strip_suffix(".rar") {
        return Some((join(stem), "rar-old", 0));
    }
    if let Some((stem, e)) = base.rsplit_once('.') {
        if let Some(n) = e.strip_prefix('r').and_then(num).filter(|_| e.len() == 3) {
            return Some((join(stem), "rar-old", n + 1));
        }
        // name.zip + name.z01, ...
        if let Some(n) = e.strip_prefix('z').and_then(num).filter(|_| e.len() == 3) {
            return Some((join(stem), "zip", n));
        }
        if e == "zip" {
            return Some((join(stem), "zip", 999_999));
        }
        // name.7z.001 / name.iso.001 / name.pkg.001
        if e.len() == 3 && num(e).is_some() {
            let n = num(e)?;
            let kind = if stem.ends_with(".7z") { "7z" } else { "split" };
            return Some((join(stem), kind, n));
        }
        // game.pkg.part0 / game.pkg.0
        if let Some(n) = e.strip_prefix("part").and_then(num) {
            if stem.ends_with(".pkg") {
                return Some((join(stem), "pkg", n));
            }
            return Some((join(stem), "split", n));
        }
    }
    // game_0.pkg, game_1.pkg
    if let Some(s) = base.strip_suffix(".pkg")
        && let Some((stem, n)) = s.rsplit_once('_')
        && let Some(n) = num(n)
    {
        return Some((join(&format!("{stem}.pkg")), "pkg", n));
    }
    // FAT32 split NSP: game.nsp/00, game.nsp/01
    if (dir.ends_with(".nsp") || dir.ends_with(".xci"))
        && let Some(n) = num(base).filter(|_| base.len() == 2)
    {
        return Some((dir.to_owned(), "nsp", n));
    }
    None
}

fn splits(idx: &mut Idx) {
    let fs = &idx.fs;
    let mut groups: HashMap<(String, &'static str), Vec<(u32, usize)>> = HashMap::new();
    for (i, f) in fs.iter().enumerate() {
        let Some((stem, kind, n)) = part_of(&f.lower) else {
            continue;
        };
        let full = groups.len() >= 4096;
        match groups.get_mut(&(stem.clone(), kind)) {
            Some(g) if g.len() < MAX_PARTS => g.push((n, i)),
            Some(_) => {}
            None if !full => {
                groups.insert((stem, kind), vec![(n, i)]);
            }
            None => {}
        }
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut pending = Vec::new();
    for ((stem, kind), mut parts) in groups {
        // A lone .rar or .zip is not a split; old-style rar and zip need their volumes.
        let volumes = match kind {
            "rar-old" => parts.iter().filter(|p| p.0 > 0).count(),
            "zip" => parts.iter().filter(|p| p.0 != 999_999).count(),
            _ => parts.len().saturating_sub(1),
        };
        if volumes == 0 {
            continue;
        }
        parts.sort_by_key(|p| p.0);
        // zip: .z01 .. .zNN then the .zip (999999 sorts last), as the format wants.
        let names: Vec<String> = parts
            .iter()
            .filter_map(|(_, i)| fs.get(*i).map(|f| f.path.replace('\\', "/")))
            .collect();
        let total = parts
            .iter()
            .filter_map(|(_, i)| fs.get(*i).map(|f| f.size))
            .fold(0u64, u64::saturating_add);
        let consecutive = parts
            .windows(2)
            .all(|w| matches!(w, [a, b] if b.0 == a.0 + 1 || b.0 == 999_999));
        let (label, desc, platform) = match kind {
            "pkg" => (
                "game.pkg.split",
                "Split PS4 / PS5 package (parts in order)",
                None,
            ),
            "nsp" => (
                "game.switch.nsp.split",
                "FAT32-split Switch package",
                Some(Platform::Switch),
            ),
            _ => (
                "archive.multipart",
                "Multi-volume archive / split file",
                None,
            ),
        };
        let first = parts.first().map(|p| p.0).unwrap_or(0);
        let starts_right = match kind {
            "rar" | "7z" | "split" => first <= 1,
            _ => first == 0 || kind == "zip",
        };
        let conf = if consecutive && starts_right { 92 } else { 80 };
        let root = idx.original_root(&stem);
        let l = Listing {
            label,
            description: desc,
            platform,
            confidence: conf,
            title_id: names.first().and_then(|n| sony_id_in(n)),
            root,
            split: (label == "archive.multipart").then_some(kind),
            metadata: names.first().cloned().into_iter().collect(),
            parts: names,
            total_size: total,
        };
        pending.push((l, format!("{stem}\0{kind}")));
    }
    for (l, key) in pending {
        idx.add(l, &key);
    }
}

/// Maps a range of the concatenated file onto split parts of the given sizes: (part index,
/// offset inside the part, length) pieces, in order. Bytes past the last part are dropped.
pub fn split_range(part_sizes: &[u64], offset: u64, len: u64) -> Vec<(usize, u64, u64)> {
    let mut out = Vec::new();
    let (mut start, mut left) = (offset, len);
    let mut base = 0u64;
    for (i, &size) in part_sizes.iter().enumerate() {
        let end = base.saturating_add(size);
        if left == 0 {
            break;
        }
        if start < end && size > 0 {
            let inside = start - base;
            let n = left.min(size - inside);
            out.push((i, inside, n));
            start = start.saturating_add(n);
            left -= n;
        }
        base = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_across_parts() {
        assert_eq!(
            split_range(&[10, 10, 10], 5, 10),
            vec![(0, 5, 5), (1, 0, 5)]
        );
        assert_eq!(split_range(&[10, 10], 25, 10), vec![]);
        assert_eq!(
            split_range(&[10, 0, 10], 8, 100),
            vec![(0, 8, 2), (2, 0, 10)]
        );
        // Saturating: sizes past u64::MAX never panic.
        assert_eq!(
            split_range(&[u64::MAX, 5], u64::MAX - 1, 4),
            vec![(0, u64::MAX - 1, 1)]
        );
    }

    #[test]
    fn ids_in_paths() {
        assert_eq!(
            sony_id_in("CUSA00207-app/sce_sys").as_deref(),
            Some("CUSA00207")
        );
        assert_eq!(sony_id_in("x/PPSA12544").as_deref(), Some("PPSA12544"));
        assert_eq!(sony_id_in("XCUSA002071/x"), None);
    }
}
