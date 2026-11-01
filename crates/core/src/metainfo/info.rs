//! Info dictionary → name, files, sizes (docs/DHT.md §4 step 6, D56), from BEP 3 (`name`,
//! `piece length`, `pieces`, `length` | `files[].{length,path}`), BEP 27 (`private`), BEP 47
//! (`attr` with `p` = padding file, path optional on padding files) and BEP 52 (`meta version`,
//! `file tree`).
//!
//! The input was verified against the infohash, but its content is still chosen by whoever made
//! the torrent: nothing here panics or logs, and the strings it builds are priced in real
//! (decoded) bytes before they are allocated, against an absolute cap ([`output_max`], D110).
//! **Nothing here may log a name or path** (docs/DHT.md §7.1: the blocklist runs after this).

use crate::liveness::bencode::{self, Value};

/// BEP 47 recommends `[".pad", "N"]` as a padding file's path.
const PAD_DIR: &str = ".pad";
/// BitComet's padding files, from before BEP 47 (`_____padding_file_N_…`).
const BITCOMET_PAD: &str = "_____padding_file_";

/// Longest name and display path kept, in bytes (D110): the Nostr title cap
/// (`nip35::MAX_TITLE_BYTES`). Real names are well under 300 bytes; a longer one is cut on a
/// character boundary so the title, its tsvector and the release parser stay small. The
/// blocklist input ([`InfoSummary::all_names`]) keeps the full strings.
pub const DISPLAY_MAX: usize = 1024;
/// Longest BEP 52 file-tree path joined, in raw bytes with separators (D110): Linux `PATH_MAX`,
/// so no client could write a longer one. Longer → [`InfoError::TooComplex`].
pub const TREE_PATH_MAX: usize = 4096;
/// Real bytes of strings a parse may build per byte of `metadata_max` (D110): every name,
/// `all_names` entry and display path, measured after lossy UTF-8 decoding (an invalid byte
/// becomes the 3-byte U+FFFD) and before it is allocated. Absolute: padding a dict does not
/// raise it. A real dict needs its whole paths once for the blocklist plus up to
/// [`DISPLAY_MAX`] per displayed file: ~4.7× its size for a v2 manga pack with long directory
/// names (6800 files, 547 KB, `a_manga_pack_parses`), so at the default 16 MiB a v2 dict of
/// that shape fits up to ~10 MB. One huge directory key over many leaves asked for terabytes.
pub const OUTPUT_FACTOR: usize = 3;
/// Floor of [`output_max`], so small `metadata_max` settings still parse ordinary dicts.
const OUTPUT_MIN: usize = 4 * 1024 * 1024;
/// Bytes charged per string built on top of its contents (the `String` header and its `Vec`
/// slot), so strings that decode to nothing (all NUL) are not free.
const STRING_COST: usize = 32;
/// The default `MAGNETAR_DHT_METADATA_MAX` (16 MiB), the cap [`parse_info`] uses.
const DEFAULT_METADATA_MAX: usize = 16 * 1024 * 1024;

/// Most real bytes of strings one parse may build when dicts are at most `metadata_max` bytes
/// (D110): [`OUTPUT_FACTOR`] × `metadata_max`, at least 4 MiB. The fetcher reserves it in each
/// decode's price.
pub const fn output_max(metadata_max: usize) -> usize {
    let n = metadata_max.saturating_mul(OUTPUT_FACTOR);
    if n < OUTPUT_MIN { OUTPUT_MIN } else { n }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoFile {
    pub path: String,
    pub size: u64,
}

/// What the crawler keeps from a verified info dict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoSummary {
    /// `name.utf-8` when it is valid UTF-8, else `name` (lossy UTF-8). NUL removed.
    pub name: String,
    /// Display files (padding files dropped), in torrent order. All of them: the caller caps.
    pub files: Vec<InfoFile>,
    /// Sum of the display files' sizes.
    pub total_size: u64,
    pub piece_length: Option<i64>,
    /// BEP 27 `private = 1`.
    pub private: bool,
    /// BEP 52 `meta version` (2 for v2 and hybrid torrents).
    pub meta_version: Option<i64>,
    /// v1 `pieces` present (BEP 3): the dict has a v1 identity.
    pub has_pieces: bool,
    pub has_file_tree: bool,
    /// Every name and path variant in the dict, displayed or not (`name`, `name.utf-8`, every
    /// `path` / `path.utf-8`, padding paths, `file tree` keys): the blocklist input (§7.2).
    pub all_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoError {
    /// Not bencode, or not a dict.
    Decode,
    /// The same key twice in one dict, at any depth (other clients may show a later value
    /// than our first-value decoder, docs/DHT.md §4 step 6).
    DuplicateKeys,
    /// `meta version` above 2 (BEP 52: check it first, refuse what we cannot handle).
    UnknownVersion,
    NoName,
    /// `files`/`length`/`file tree` missing or malformed (negative length, missing path, …).
    BadFiles,
    /// Total size 0 (only padding or empty files).
    Empty,
    /// More bencode values than a decode may build (memory amplification, docs/DHT.md §3.8).
    TooComplex,
}

impl InfoError {
    pub fn code(self) -> &'static str {
        match self {
            InfoError::Decode => "decode",
            InfoError::DuplicateKeys => "duplicate_keys",
            InfoError::UnknownVersion => "meta_version",
            InfoError::NoName => "no_name",
            InfoError::BadFiles => "bad_files",
            InfoError::Empty => "empty",
            InfoError::TooComplex => "too_complex",
        }
    }

    /// The error a [`code`](Self::code) names (worker reports, D97).
    pub fn from_code(code: &str) -> Option<Self> {
        [
            InfoError::Decode,
            InfoError::DuplicateKeys,
            InfoError::UnknownVersion,
            InfoError::NoName,
            InfoError::BadFiles,
            InfoError::Empty,
            InfoError::TooComplex,
        ]
        .into_iter()
        .find(|e| e.code() == code)
    }
}

/// Length of [`text`]`(b)` without allocating: lossy UTF-8 (one U+FFFD, 3 bytes, per invalid
/// sequence, as `String::from_utf8_lossy`), NUL removed.
fn text_len(b: &[u8]) -> usize {
    b.utf8_chunks()
        .map(|c| {
            let nul = c.valid().bytes().filter(|&x| x == 0).count();
            c.valid().len() - nul + if c.invalid().is_empty() { 0 } else { 3 }
        })
        .sum()
}

/// Push `s` while `out` stays within `max` bytes, cut on a character boundary. False once
/// something was cut (nothing more may follow, or the result would not be a prefix).
fn push_capped(out: &mut String, s: &str, max: usize) -> bool {
    let room = max.saturating_sub(out.len());
    if s.len() <= room {
        out.push_str(s);
        return true;
    }
    let mut end = room;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&s[..end]);
    false
}

/// Append [`text`]`(b)` to `out`, stopping at `max` bytes. False once cut.
fn push_text(out: &mut String, b: &[u8], max: usize) -> bool {
    for c in b.utf8_chunks() {
        for piece in c.valid().split('\0') {
            if !push_capped(out, piece, max) {
                return false;
            }
        }
        if !c.invalid().is_empty() && !push_capped(out, "\u{FFFD}", max) {
            return false;
        }
    }
    true
}

/// Lossy UTF-8 without NUL (Postgres `text` and `jsonb` cannot hold U+0000).
fn text(b: &[u8]) -> String {
    let mut s = String::with_capacity(text_len(b));
    push_text(&mut s, b, usize::MAX);
    s
}

/// True when `b` is valid UTF-8 (then [`text`] keeps it as is, NUL removed).
fn is_utf8(b: &[u8]) -> bool {
    std::str::from_utf8(b).is_ok()
}

/// The real bytes a parse may still build, spent before each string is allocated (D110).
struct Budget(usize);

impl Budget {
    /// Take one string of `n` bytes, or fail before anything is allocated.
    fn spend(&mut self, n: usize) -> Result<(), InfoError> {
        self.0 = self
            .0
            .checked_sub(n.saturating_add(STRING_COST))
            .ok_or(InfoError::TooComplex)?;
        Ok(())
    }
}

/// True when some dict in the tree has a key twice.
fn has_duplicate_keys(v: &Value) -> bool {
    // Iterative walk: the decoder already bounds depth, this also bounds the stack.
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Dict(entries) => {
                let mut keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
                keys.sort_unstable();
                if keys.windows(2).any(|w| w[0] == w[1]) {
                    return true;
                }
                stack.extend(entries.iter().map(|(_, v)| v));
            }
            Value::List(items) => stack.extend(items.iter()),
            _ => {}
        }
    }
    false
}

/// A path list (`path` / `path.utf-8`) as strings, None when it is not a list of strings.
fn path_parts(v: &Value) -> Option<Vec<&[u8]>> {
    v.as_list()?.iter().map(Value::as_bytes).collect()
}

/// Length of [`join`]`(parts, usize::MAX)`, without allocating.
fn join_len(parts: &[&[u8]]) -> usize {
    parts.iter().map(|p| text_len(p)).sum::<usize>() + parts.len().saturating_sub(1)
}

/// The parts as lossy text joined by `/`, at most `max` bytes (cut on a character boundary).
fn join(parts: &[&[u8]], max: usize) -> String {
    let mut out = String::with_capacity(join_len(parts).min(max));
    for (i, p) in parts.iter().enumerate() {
        if (i > 0 && !push_capped(&mut out, "/", max)) || !push_text(&mut out, p, max) {
            break;
        }
    }
    out
}

/// Price and build one full `all_names` path.
fn name_path(parts: &[&[u8]], budget: &mut Budget) -> Result<String, InfoError> {
    budget.spend(join_len(parts))?;
    Ok(join(parts, usize::MAX))
}

/// Price and build one display path, capped at [`DISPLAY_MAX`].
fn display_path(parts: &[&[u8]], budget: &mut Budget) -> Result<String, InfoError> {
    budget.spend(join_len(parts).min(DISPLAY_MAX))?;
    Ok(join(parts, DISPLAY_MAX))
}

/// Cut `s` to at most `max` bytes on a character boundary (no-op when it fits).
pub fn truncate_utf8(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

fn is_padding(attr: Option<&[u8]>, first: Option<&[u8]>, last: Option<&[u8]>) -> bool {
    attr.is_some_and(|a| a.contains(&b'p'))
        || first == Some(PAD_DIR.as_bytes())
        || last.is_some_and(|l| l.starts_with(BITCOMET_PAD.as_bytes()))
}

/// Parse a verified info dict, building at most [`output_max`] of the default 16 MiB
/// `metadata_max` in strings.
pub fn parse_info(raw: &[u8]) -> Result<InfoSummary, InfoError> {
    parse_info_bounded(raw, output_max(DEFAULT_METADATA_MAX))
}

/// Parse a verified info dict, building at most `output_max` real bytes of strings (names,
/// `all_names`, display paths); above it [`InfoError::TooComplex`], before the string that
/// would cross it is allocated (D110).
pub fn parse_info_bounded(raw: &[u8], output_max: usize) -> Result<InfoSummary, InfoError> {
    let mut budget = Budget(output_max);
    let v = bencode::decode(raw).map_err(|_| InfoError::Decode)?;
    if v.as_dict().is_none() {
        return Err(InfoError::Decode);
    }
    if has_duplicate_keys(&v) {
        return Err(InfoError::DuplicateKeys);
    }
    let meta_version = v.get(b"meta version").and_then(Value::as_int);
    if meta_version.is_some_and(|m| m > 2) {
        return Err(InfoError::UnknownVersion);
    }
    let mut all_names = Vec::new();
    let name_raw = v.get(b"name").and_then(Value::as_bytes);
    let name_utf8 = v.get(b"name.utf-8").and_then(Value::as_bytes);
    for n in [name_raw, name_utf8].into_iter().flatten() {
        budget.spend(text_len(n))?;
        all_names.push(text(n));
    }
    // `name.utf-8` when it is valid UTF-8, else `name` lossy; capped as it is built (D110).
    let name = name_utf8
        .filter(|n| is_utf8(n))
        .or(name_raw)
        .map(|n| {
            budget.spend(text_len(n).min(DISPLAY_MAX))?;
            let mut s = String::new();
            push_text(&mut s, n, DISPLAY_MAX);
            Ok(s)
        })
        .transpose()?
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or(InfoError::NoName)?;
    let private = v.get(b"private").and_then(Value::as_int) == Some(1);
    let piece_length = v.get(b"piece length").and_then(Value::as_int);
    let has_pieces = v.get(b"pieces").and_then(Value::as_bytes).is_some();
    let tree = v.get(b"file tree").filter(|t| t.as_dict().is_some());

    let mut files = Vec::new();
    if let Some(list) = v.get(b"files") {
        let list = list.as_list().ok_or(InfoError::BadFiles)?;
        for f in list {
            let len = f.get(b"length").and_then(Value::as_int);
            let attr = f.get(b"attr").and_then(Value::as_bytes);
            let parts = f
                .get(b"path")
                .map(|p| path_parts(p).ok_or(InfoError::BadFiles));
            let parts = parts.transpose()?;
            let parts_utf8 = f.get(b"path.utf-8").and_then(path_parts);
            let first = parts.as_ref().and_then(|p| p.first().copied());
            let last = parts.as_ref().and_then(|p| p.last().copied());
            for p in [&parts, &parts_utf8].into_iter().flatten() {
                all_names.push(name_path(p, &mut budget)?);
            }
            if is_padding(attr, first, last) {
                // BEP 47: a padding file's path may be absent; its length is still required by
                // BEP 3 but carries no content.
                continue;
            }
            let size = len
                .and_then(|l| u64::try_from(l).ok())
                .ok_or(InfoError::BadFiles)?;
            let parts = parts.filter(|p| !p.is_empty()).ok_or(InfoError::BadFiles)?;
            // `path.utf-8` when every element is valid UTF-8, else `path` lossy.
            let shown = parts_utf8
                .as_deref()
                .filter(|p| !p.is_empty() && p.iter().all(|e| is_utf8(e)))
                .unwrap_or(&parts);
            let path = display_path(shown, &mut budget)?;
            files.push(InfoFile { path, size });
        }
    } else if let Some(len) = v.get(b"length") {
        let size = len
            .as_int()
            .and_then(|l| u64::try_from(l).ok())
            .ok_or(InfoError::BadFiles)?;
        budget.spend(name.len())?;
        files.push(InfoFile {
            path: name.clone(),
            size,
        });
    } else if let Some(t) = tree {
        // v2-only dicts: the file tree is the only file list.
        walk_tree(
            t,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut files,
            &mut all_names,
            &mut budget,
            0,
        )?;
    } else {
        return Err(InfoError::BadFiles);
    }
    if let Some(t) = tree.filter(|_| v.get(b"files").is_some() || v.get(b"length").is_some()) {
        // Hybrid: the v1 list is displayed, the v2 keys still go to the blocklist. A tree that
        // does not walk is an invalid hybrid (the screen must see every name, not a prefix).
        let mut ignored = Vec::new();
        walk_tree(
            t,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut ignored,
            &mut all_names,
            &mut budget,
            0,
        )?;
    }
    let total_size = files
        .iter()
        .try_fold(0u64, |acc, f| acc.checked_add(f.size))
        .ok_or(InfoError::BadFiles)?;
    // Stored as a Postgres bigint.
    if i64::try_from(total_size).is_err() {
        return Err(InfoError::BadFiles);
    }
    if total_size == 0 {
        return Err(InfoError::Empty);
    }
    // Display strings were built capped (D110); the screen has the full ones in all_names.
    Ok(InfoSummary {
        name,
        files,
        total_size,
        piece_length,
        private,
        meta_version,
        has_pieces,
        has_file_tree: tree.is_some(),
        all_names,
    })
}

/// BEP 52 `file tree`: dict keys are path elements; a key `""` holding `length` marks a file.
/// Every leaf joins its whole prefix, so each string is priced in real bytes first (D110): at
/// most [`TREE_PATH_MAX`] raw bytes per path and the parse's [`Budget`] in all, else
/// [`InfoError::TooComplex`]. `lens` holds each prefix element's decoded length, so a leaf is
/// priced in O(depth).
fn walk_tree<'a>(
    node: &'a Value,
    prefix: &mut Vec<&'a [u8]>,
    lens: &mut Vec<usize>,
    files: &mut Vec<InfoFile>,
    names: &mut Vec<String>,
    budget: &mut Budget,
    depth: usize,
) -> Result<(), InfoError> {
    // The decoder caps nesting at 64; this keeps our own recursion bounded the same way.
    if depth > 64 {
        return Err(InfoError::BadFiles);
    }
    let entries = node.as_dict().ok_or(InfoError::BadFiles)?;
    for (k, child) in entries {
        if k.is_empty() {
            // The properties of the path so far: a file when it has `length`.
            if prefix.is_empty() {
                return Err(InfoError::BadFiles); // "the root … must not be a file"
            }
            let len = child
                .get(b"length")
                .and_then(Value::as_int)
                .and_then(|l| u64::try_from(l).ok())
                .ok_or(InfoError::BadFiles)?;
            let attr = child.get(b"attr").and_then(Value::as_bytes);
            // Raw and decoded lengths of the joined path (depth ≤ 64, so these sums are cheap).
            let seps = prefix.len() - 1;
            if prefix.iter().map(|p| p.len()).sum::<usize>() + seps > TREE_PATH_MAX {
                return Err(InfoError::TooComplex);
            }
            let real_len = lens.iter().sum::<usize>() + seps;
            let padding = is_padding(attr, prefix.first().copied(), prefix.last().copied());
            // The whole path too, as stored for v1 files (a screen rule may span elements).
            budget.spend(real_len)?;
            names.push(join(prefix, usize::MAX));
            if !padding {
                budget.spend(real_len.min(DISPLAY_MAX))?;
                files.push(InfoFile {
                    path: join(prefix, DISPLAY_MAX),
                    size: len,
                });
            }
            continue;
        }
        let n = text_len(k);
        budget.spend(n)?;
        names.push(text(k));
        prefix.push(k);
        lens.push(n);
        let r = walk_tree(child, prefix, lens, files, names, budget, depth + 1);
        prefix.pop();
        lens.pop();
        r?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn single_file() {
        let s = parse_info(b"d6:lengthi1000e4:name9:movie.mkv12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae").unwrap();
        assert_eq!(s.name, "movie.mkv");
        assert_eq!(
            s.files,
            vec![InfoFile {
                path: "movie.mkv".into(),
                size: 1000
            }]
        );
        assert_eq!(s.total_size, 1000);
        assert_eq!(s.piece_length, Some(16384));
        assert!(s.has_pieces && !s.private && s.meta_version.is_none());
    }

    #[test]
    fn multi_file_padding_and_utf8() {
        let raw = b"d5:filesld6:lengthi10e4:pathl3:dir5:a.txteed4:attr1:p6:lengthi6e4:pathl4:.pad1:6eed6:lengthi4e4:pathl4:.pad1:4eed6:lengthi5e4:pathl2:\xffbe10:path.utf-8l2:okeee4:name3:top10:name.utf-84:Top!12:piece lengthi16e6:pieces0:e";
        let s = parse_info(raw).unwrap();
        assert_eq!(s.name, "Top!");
        assert_eq!(
            s.files,
            vec![
                InfoFile {
                    path: "dir/a.txt".into(),
                    size: 10
                },
                InfoFile {
                    path: "ok".into(),
                    size: 5
                }
            ]
        );
        assert_eq!(s.total_size, 15);
        // Every variant reaches the blocklist input, padding paths included.
        for n in ["top", "Top!", "dir/a.txt", ".pad/6", ".pad/4", "ok"] {
            assert!(s.all_names.iter().any(|x| x == n), "{n}");
        }
    }

    #[test]
    fn rejects() {
        // duplicate key at depth 2
        assert_eq!(
            parse_info(b"d5:filesld6:lengthi1e6:lengthi2e4:pathl1:aeee4:name1:xe"),
            Err(InfoError::DuplicateKeys)
        );
        // duplicate name at the top
        assert_eq!(
            parse_info(b"d6:lengthi1e4:name1:a4:name1:be"),
            Err(InfoError::DuplicateKeys)
        );
        assert_eq!(parse_info(b"d6:lengthi1ee"), Err(InfoError::NoName));
        assert_eq!(
            parse_info(b"d6:lengthi1e4:name2:  e"),
            Err(InfoError::NoName)
        );
        assert_eq!(
            parse_info(b"d6:lengthi-1e4:name1:ae"),
            Err(InfoError::BadFiles)
        );
        assert_eq!(parse_info(b"d6:lengthi0e4:name1:ae"), Err(InfoError::Empty));
        assert_eq!(parse_info(b"d4:name1:ae"), Err(InfoError::BadFiles));
        assert_eq!(
            parse_info(b"d6:lengthi1e12:meta versioni3e4:name1:ae"),
            Err(InfoError::UnknownVersion)
        );
        assert_eq!(parse_info(b"le"), Err(InfoError::Decode));
        assert_eq!(
            parse_info(b"d6:lengthi1e4:name1:aee"),
            Err(InfoError::Decode)
        );
        // sizes that overflow u64 together
        let big = format!(
            "d5:filesld6:lengthi{m}e4:pathl1:aeed6:lengthi{m}e4:pathl1:beee4:name1:xe",
            m = i64::MAX
        );
        assert_eq!(parse_info(big.as_bytes()), Err(InfoError::BadFiles));
    }

    #[test]
    fn private_and_nul() {
        let s = parse_info(b"d6:lengthi1e4:name3:a\x00b7:privatei1ee").unwrap();
        assert!(s.private);
        assert_eq!(s.name, "ab");
    }

    #[test]
    fn v2_file_tree() {
        // BEP 52 example shape: dir1/dir2/fileA.txt (1024) + a padding entry.
        let raw = b"d9:file treed4:dir1d4:dir2d9:fileA.txtd0:d6:lengthi1024e11:pieces root32:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaeeeee12:meta versioni2e4:name4:dir112:piece lengthi16384ee";
        let s = parse_info(raw).unwrap();
        assert_eq!(s.meta_version, Some(2));
        assert!(!s.has_pieces && s.has_file_tree);
        assert_eq!(
            s.files,
            vec![InfoFile {
                path: "dir1/dir2/fileA.txt".into(),
                size: 1024
            }]
        );
        // The whole path reaches the screen too, not only its elements.
        assert!(s.all_names.iter().any(|n| n == "dir1/dir2/fileA.txt"));
        // A hybrid whose tree does not walk is invalid (the screen must see every name).
        assert_eq!(
            parse_info(b"d9:file treed1:ad0:d6:lengthi-1eeee6:lengthi5e4:name1:a6:pieces0:e"),
            Err(InfoError::BadFiles)
        );
        // root must not be a file
        assert_eq!(
            parse_info(b"d9:file treed0:d6:lengthi1eee4:name1:ae"),
            Err(InfoError::BadFiles)
        );
    }

    /// A v2-only info dict whose file tree is `tree` (a dict value).
    fn v2_with_tree(tree: Value) -> Vec<u8> {
        bencode::encode(&Value::Dict(vec![
            (b"file tree".to_vec(), tree),
            (b"meta version".to_vec(), Value::Int(2)),
            (b"name".to_vec(), Value::Bytes(b"t".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
        ]))
    }

    /// A tree leaf: `{"": {length, pieces root}}`.
    fn leaf(len: i64) -> Value {
        Value::Dict(vec![(
            Vec::new(),
            Value::Dict(vec![
                (b"length".to_vec(), Value::Int(len)),
                (b"pieces root".to_vec(), Value::Bytes(vec![b'r'; 32])),
            ]),
        )])
    }

    /// `n` leaves named 0000, 0001, … under `dir`.
    fn wide_dir(dir: Vec<u8>, n: usize) -> Value {
        let leaves = (0..n)
            .map(|i| (format!("{i:04}").into_bytes(), leaf(1)))
            .collect();
        Value::Dict(vec![(dir, Value::Dict(leaves))])
    }

    /// Real bytes of every string a summary holds.
    fn built(s: &InfoSummary) -> usize {
        s.name.len()
            + s.files.iter().map(|f| f.path.len()).sum::<usize>()
            + s.all_names.iter().map(String::len).sum::<usize>()
    }

    /// A hybrid dict: `tree` as its file tree, one v1 file, and `pieces` of `pad` bytes.
    fn hybrid_with_tree(tree: Value, pad: usize) -> Vec<u8> {
        bencode::encode(&Value::Dict(vec![
            (b"file tree".to_vec(), tree),
            (b"length".to_vec(), Value::Int(5)),
            (b"name".to_vec(), Value::Bytes(b"a".to_vec())),
            (b"pieces".to_vec(), Value::Bytes(vec![b'p'; pad])),
        ]))
    }

    #[test]
    fn file_tree_output_is_bounded_before_joining() {
        // D110, the audit's shape: one big directory key over many tiny leaves. Each leaf
        // used to copy the key twice (16 MB here from a ~150 KB dict; terabytes at 16 MiB).
        let cap = output_max(0);
        let raw = v2_with_tree(wide_dir(vec![b'k'; 4000], 2000));
        assert!(raw.len() < 256 * 1024);
        assert_eq!(parse_info_bounded(&raw, cap), Err(InfoError::TooComplex));
        // One path longer than PATH_MAX is refused on its own.
        let raw = v2_with_tree(wide_dir(vec![b'k'; TREE_PATH_MAX], 1));
        assert_eq!(parse_info(&raw), Err(InfoError::TooComplex));
        // A hybrid's tree is priced too (it feeds the blocklist input).
        let raw = hybrid_with_tree(wide_dir(vec![b'k'; 4000], 2000), 0);
        assert_eq!(parse_info_bounded(&raw, cap), Err(InfoError::TooComplex));
    }

    #[test]
    fn output_is_capped_in_real_bytes() {
        // D110 review: the cap counts decoded bytes (0xFF → U+FFFD, 3 bytes) and does not grow
        // with the dict, so neither invalid UTF-8 nor `pieces` padding buys more output.
        let cap = output_max(0);
        let check = |raw: &[u8]| match parse_info_bounded(raw, cap) {
            Ok(s) => panic!("accepted, {} bytes built for a {cap} cap", built(&s)),
            Err(e) => assert_eq!(e, InfoError::TooComplex),
        };
        // 44 bytes of 0xFF over 20 000 leaves: ~1.5 MB of dict, ~6 MB of strings.
        let raw = v2_with_tree(wide_dir(vec![0xFF; 44], 20_000));
        assert!(raw.len() < cap / 2);
        check(&raw);
        // A hybrid padded with 3 MB of `pieces`, a 1000-byte 0xFF directory over 1500 leaves.
        let raw = hybrid_with_tree(wide_dir(vec![0xFF; 1000], 1500), 3 << 20);
        check(&raw);
        // Whatever is accepted stays under the cap.
        let raw = v2_with_tree(wide_dir(vec![0xFF; 44], 5000));
        let s = parse_info_bounded(&raw, cap).unwrap();
        assert_eq!(s.files.len(), 5000);
        assert!(built(&s) <= cap);
        assert!(s.files[0].path.starts_with('\u{FFFD}'));
        // The default cap is reserved by the fetcher's decode price.
        assert_eq!(output_max(16 << 20), OUTPUT_FACTOR * (16 << 20));
    }

    #[test]
    fn a_manga_pack_parses() {
        // D110 review: a v2-only manga pack, long directory names over short page names (each
        // leaf repeats ~190 bytes of directories), was refused by the raw-size budget.
        let root = "Shingeki no Kyojin - Attack on Titan (2010-2021) (Complete Series) (Digital) (danke-Empire)";
        let vols = (1..=34)
            .map(|v| {
                let name = format!(
                    "Shingeki no Kyojin - Attack on Titan v{v:02} (2012) (Digital) (danke-Empire) [Kodansha Comics]"
                );
                let pages = (1..=200)
                    .map(|p| (format!("{p:03}.jpg").into_bytes(), leaf(400_000)))
                    .collect();
                (name.into_bytes(), Value::Dict(pages))
            })
            .collect();
        let tree = Value::Dict(vec![(root.as_bytes().to_vec(), Value::Dict(vols))]);
        let raw = v2_with_tree(tree);
        assert!(raw.len() < 600 * 1024);
        let s = parse_info(&raw).unwrap();
        assert_eq!(s.files.len(), 34 * 200);
        assert!(s.files[0].path.ends_with("[Kodansha Comics]/001.jpg"));
        assert!(built(&s) <= output_max(16 << 20));
    }

    #[test]
    fn real_file_trees_fit_the_budget() {
        // A season pack: 3000 files under two shared directories, and one deep long path.
        let season = Value::Dict(vec![(
            b"Season 01".to_vec(),
            Value::Dict(
                (0..3000)
                    .map(|i| {
                        let name = format!("Some.Show.2020.S01E{i:04}.1080p.WEB-DL.x264-GRP.mkv");
                        (name.into_bytes(), leaf(1_000_000))
                    })
                    .collect(),
            ),
        )]);
        let tree = Value::Dict(vec![(
            b"Some Show (2020) Complete [1080p WEB-DL]".to_vec(),
            season,
        )]);
        let s = parse_info(&v2_with_tree(tree)).unwrap();
        assert_eq!(s.files.len(), 3000);
        assert!(
            s.files[0]
                .path
                .starts_with("Some Show (2020) Complete [1080p WEB-DL]/Season 01/")
        );
        // 60 nested 64-byte directories (the decoder's depth limit): a 3.9 KB path, under
        // TREE_PATH_MAX.
        let mut node = leaf(7);
        for i in 0..60 {
            node = Value::Dict(vec![(
                format!("{i:02}{}", "d".repeat(62)).into_bytes(),
                node,
            )]);
        }
        let s = parse_info(&v2_with_tree(node)).unwrap();
        assert_eq!(s.files.len(), 1);
        // The display path is capped, the blocklist input keeps the whole path.
        assert_eq!(s.files[0].path.len(), DISPLAY_MAX);
        assert!(s.all_names.iter().any(|n| n.len() == 60 * 64 + 59));
    }

    #[test]
    fn name_and_paths_are_capped_on_a_char_boundary() {
        // D110: the DHT title and each display path stop at DISPLAY_MAX bytes.
        let long = "é".repeat(DISPLAY_MAX); // 2 bytes each
        let raw = bencode::encode(&Value::Dict(vec![
            (
                b"files".to_vec(),
                Value::List(vec![Value::Dict(vec![
                    (b"length".to_vec(), Value::Int(5)),
                    (
                        b"path".to_vec(),
                        Value::List(vec![Value::Bytes(format!("x{long}").into_bytes())]),
                    ),
                ])]),
            ),
            (b"name".to_vec(), Value::Bytes(long.clone().into_bytes())),
        ]));
        let s = parse_info(&raw).unwrap();
        assert_eq!(s.name.len(), DISPLAY_MAX);
        assert!(s.name.chars().all(|c| c == 'é'));
        // "x" + é…: the cut falls inside an é and backs off one byte.
        assert_eq!(s.files[0].path.len(), DISPLAY_MAX - 1);
        // The screen still sees every byte.
        assert!(s.all_names.contains(&long));
        assert!(s.all_names.contains(&format!("x{long}")));
        // A single-file torrent's path is its name: capped the same way.
        let raw = format!(
            "d6:lengthi1e4:name{}:{}e",
            3 * DISPLAY_MAX,
            "a".repeat(3 * DISPLAY_MAX)
        );
        let s = parse_info(raw.as_bytes()).unwrap();
        assert_eq!(
            (s.name.len(), s.files[0].path.len()),
            (DISPLAY_MAX, DISPLAY_MAX)
        );
        let mut t = String::from("aé");
        truncate_utf8(&mut t, 2);
        assert_eq!(t, "a");
    }

    proptest! {
        #[test]
        fn priced_lengths_match_what_is_built(
            parts in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..40), 0..5),
            max in 0usize..120,
        ) {
            // The price is the real decoded length (lossy UTF-8, NUL removed), and a capped
            // join is a char-boundary prefix of the full one.
            let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
            for p in &parts {
                let want = String::from_utf8_lossy(p).replace('\0', "");
                prop_assert_eq!(text(p), want.clone());
                prop_assert_eq!(text_len(p), want.len());
            }
            let full = join(&parts, usize::MAX);
            prop_assert_eq!(full.len(), join_len(&parts));
            let mut want = full.clone();
            truncate_utf8(&mut want, max);
            prop_assert_eq!(join(&parts, max), want);
        }

        #[test]
        fn arbitrary_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = parse_info(&b);
            prop_assert!(bencode::count_values(&b, 64) <= 65);
        }

        #[test]
        fn mutated_info_never_panics(cut in 0usize..200, flip in any::<u8>()) {
            let mut raw = b"d5:filesld6:lengthi10e4:pathl3:dir5:a.txteed4:attr1:p6:lengthi6e4:pathl4:.pad1:6eee9:file treed1:ad0:d6:lengthi3eee4:name3:top12:piece lengthi16e6:pieces0:e".to_vec();
            let i = cut % raw.len();
            raw[i] ^= flip;
            let _ = parse_info(&raw);
        }
    }
}
