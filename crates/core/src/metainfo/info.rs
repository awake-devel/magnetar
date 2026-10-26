//! Info dictionary → name, files, sizes (docs/DHT.md §4 step 6, D56), from BEP 3 (`name`,
//! `piece length`, `pieces`, `length` | `files[].{length,path}`), BEP 27 (`private`), BEP 47
//! (`attr` with `p` = padding file, path optional on padding files) and BEP 52 (`meta version`,
//! `file tree`).
//!
//! The input was verified against the infohash, but its content is still chosen by whoever made
//! the torrent: nothing here panics, logs or allocates beyond a small multiple of the input.
//! **Nothing here may log a name or path** (docs/DHT.md §7.1: the blocklist runs after this).

use crate::liveness::bencode::{self, Value};

/// BEP 47 recommends `[".pad", "N"]` as a padding file's path.
const PAD_DIR: &str = ".pad";
/// BitComet's padding files, from before BEP 47 (`_____padding_file_N_…`).
const BITCOMET_PAD: &str = "_____padding_file_";

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
        }
    }
}

/// Lossy UTF-8 without NUL (Postgres `text` and `jsonb` cannot hold U+0000).
fn text(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.into_owned()
    }
}

fn strict_text(b: &[u8]) -> Option<String> {
    std::str::from_utf8(b).ok().map(|s| s.replace('\0', ""))
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

fn join(parts: &[&[u8]], strict: bool) -> Option<String> {
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        if strict {
            out.push_str(&strict_text(p)?);
        } else {
            out.push_str(&text(p));
        }
    }
    Some(out)
}

fn is_padding(attr: Option<&[u8]>, first: Option<&[u8]>, last: Option<&[u8]>) -> bool {
    attr.is_some_and(|a| a.contains(&b'p'))
        || first == Some(PAD_DIR.as_bytes())
        || last.is_some_and(|l| l.starts_with(BITCOMET_PAD.as_bytes()))
}

/// Parse a verified info dict.
pub fn parse_info(raw: &[u8]) -> Result<InfoSummary, InfoError> {
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
    if let Some(n) = name_raw {
        all_names.push(text(n));
    }
    if let Some(n) = name_utf8 {
        all_names.push(text(n));
    }
    let name = name_utf8
        .and_then(strict_text)
        .or_else(|| name_raw.map(text))
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
            if let Some(p) = &parts {
                all_names.push(join(p, false).unwrap_or_default());
            }
            if let Some(p) = &parts_utf8 {
                all_names.push(join(p, false).unwrap_or_default());
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
            let path = parts_utf8
                .as_deref()
                .filter(|p| !p.is_empty())
                .and_then(|p| join(p, true))
                .or_else(|| join(&parts, false))
                .ok_or(InfoError::BadFiles)?;
            files.push(InfoFile { path, size });
        }
    } else if let Some(len) = v.get(b"length") {
        let size = len
            .as_int()
            .and_then(|l| u64::try_from(l).ok())
            .ok_or(InfoError::BadFiles)?;
        files.push(InfoFile {
            path: name.clone(),
            size,
        });
    } else if let Some(t) = tree {
        // v2-only dicts: the file tree is the only file list.
        walk_tree(t, &mut Vec::new(), &mut files, &mut all_names, 0)?;
    } else {
        return Err(InfoError::BadFiles);
    }
    if let Some(t) = tree.filter(|_| v.get(b"files").is_some() || v.get(b"length").is_some()) {
        // Hybrid: the v1 list is displayed, the v2 keys still go to the blocklist.
        let mut ignored = Vec::new();
        let _ = walk_tree(t, &mut Vec::new(), &mut ignored, &mut all_names, 0);
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
fn walk_tree<'a>(
    node: &'a Value,
    prefix: &mut Vec<&'a [u8]>,
    files: &mut Vec<InfoFile>,
    names: &mut Vec<String>,
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
            if !is_padding(attr, prefix.first().copied(), prefix.last().copied()) {
                files.push(InfoFile {
                    path: join(prefix, false).unwrap_or_default(),
                    size: len,
                });
            }
            continue;
        }
        names.push(text(k));
        prefix.push(k);
        let r = walk_tree(child, prefix, files, names, depth + 1);
        prefix.pop();
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
        // root must not be a file
        assert_eq!(
            parse_info(b"d9:file treed0:d6:lengthi1eee4:name1:ae"),
            Err(InfoError::BadFiles)
        );
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = parse_info(&b);
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
