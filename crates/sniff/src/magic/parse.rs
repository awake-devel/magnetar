//! TOML signature files -> compiled rule trees.
//!
//! Format (see `data/signatures/README.md` for the full reference):
//!
//! ```toml
//! [[rule]]
//! id = "gif"                 # optional, for --explain
//! label = "image.gif"
//! confidence = 98
//! offset = 0                 # negative = from EOF; or { at = 0x3C, type = "u32le", add = 4 }
//! type = "string"
//! value = ["GIF87a", "GIF89a"]
//!   [[rule.then]]            # continuation: required unless optional = true
//!   offset = 6
//!   type = "u16le"
//!   field = "width"
//! ```

use serde::Deserialize;

/// A signature file or rule that failed to compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigError {
    pub source: String,
    pub rule: String,
    pub message: String,
}

impl std::fmt::Display for SigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: rule {}: {}", self.source, self.rule, self.message)
    }
}

impl std::error::Error for SigError {}

const MAX_DEPTH: usize = 8;
const MAX_LEN: u64 = 64 * 1024;
const MAX_SEARCH: u64 = 1 << 20;
/// Per-file limits, so a small user file cannot make every detection slow: evaluation is
/// linear in nodes and in alternatives x search window, and is paid again every round.
const MAX_RULES: usize = 4096;
const MAX_NODES: usize = 32 * 1024;
const MAX_ALTS: usize = 256;
/// Sum over search tests of alternatives x (window + longest alternative): 64 MiB is 64
/// alternatives over the largest (1 MiB) window, a few ms per pass.
const MAX_SEARCH_COST: u64 = 64 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    rule: Vec<RawNode>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawOffset {
    Abs(i64),
    Indirect(RawIndirect),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndirect {
    at: i64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    add: i64,
    #[serde(default = "one")]
    mul: u64,
}

fn one() -> u64 {
    1
}

/// One node of a rule tree; top-level-only keys are rejected on children.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    // top level only
    id: Option<String>,
    label: Option<String>,
    mime: Option<String>,
    description: Option<String>,
    confidence: Option<u8>,
    strength: Option<i32>,
    fetch: Option<bool>,
    head: Option<u64>,
    tail: Option<u64>,
    // any level
    #[serde(default)]
    offset: Option<RawOffset>,
    #[serde(default)]
    relative: bool,
    #[serde(rename = "type")]
    kind: String,
    value: Option<OneOrMany>,
    hex: Option<OneOrMany>,
    test: Option<String>,
    mask: Option<u64>,
    shift: Option<u32>,
    range: Option<u64>,
    len: Option<u64>,
    class: Option<String>,
    #[serde(default)]
    nocase: bool,
    field: Option<String>,
    #[serde(default)]
    optional: bool,
    note: Option<String>,
    #[serde(default)]
    set: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    then: Vec<RawNode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumKind {
    U8,
    U16Le,
    U16Be,
    U32Le,
    U32Be,
    U64Le,
    U64Be,
}

impl NumKind {
    fn parse(s: &str) -> Option<NumKind> {
        Some(match s {
            "u8" | "byte" => NumKind::U8,
            "u16le" | "leshort" => NumKind::U16Le,
            "u16be" | "beshort" => NumKind::U16Be,
            "u32le" | "lelong" => NumKind::U32Le,
            "u32be" | "belong" => NumKind::U32Be,
            "u64le" | "lequad" => NumKind::U64Le,
            "u64be" | "bequad" => NumKind::U64Be,
            _ => return None,
        })
    }
    pub fn width(self) -> usize {
        match self {
            NumKind::U8 => 1,
            NumKind::U16Le | NumKind::U16Be => 2,
            NumKind::U32Le | NumKind::U32Be => 4,
            NumKind::U64Le | NumKind::U64Be => 8,
        }
    }
    pub fn read(self, b: &[u8]) -> Option<u64> {
        use crate::util::*;
        Some(match self {
            NumKind::U8 => u64::from(u8at(b, 0)?),
            NumKind::U16Le => u64::from(u16le(b, 0)?),
            NumKind::U16Be => u64::from(u16be(b, 0)?),
            NumKind::U32Le => u64::from(u32le(b, 0)?),
            NumKind::U32Be => u64::from(u32be(b, 0)?),
            NumKind::U64Le => u64le(b, 0)?,
            NumKind::U64Be => u64be(b, 0)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operand {
    Lit(u64),
    /// The file length.
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmp {
    Any,
    Eq(Operand),
    Ne(Operand),
    Lt(Operand),
    Le(Operand),
    Gt(Operand),
    Ge(Operand),
    /// All bits of the mask set.
    AllSet(u64),
    /// None of the bits set.
    NoneSet(u64),
    /// `lo..hi` (exclusive) or `lo..=hi`.
    Range(u64, u64, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Printable,
    Alnum,
    UpperAlnum,
    Digit,
    Hex,
    Graphic,
    Whitespace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextKind {
    /// Exactly `len` bytes, classified as a whole; captured trimmed.
    Ascii,
    /// Up to `len` bytes, NUL-terminated; captured trimmed (lossy UTF-8).
    Cstring,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Test {
    Num {
        kind: NumKind,
        mask: Option<u64>,
        shift: u32,
        cmps: Vec<Cmp>,
    },
    Bytes {
        alts: Vec<Vec<u8>>,
        nocase: bool,
    },
    Search {
        alts: Vec<Vec<u8>>,
        range: u64,
    },
    Text {
        kind: TextKind,
        len: u64,
        class: Option<Class>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Offset {
    /// From the start (or from the parent match end when `relative`).
    Start(u64),
    /// Bytes before EOF.
    End(u64),
    /// Value read at `at` (negative = from EOF), times `mul`, plus `add`.
    Indirect {
        at: i64,
        kind: NumKind,
        add: i64,
        mul: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub offset: Offset,
    pub relative: bool,
    pub test: Test,
    pub field: Option<String>,
    pub optional: bool,
    pub label: Option<String>,
    pub confidence: Option<u8>,
    pub note: Option<String>,
    pub set: Vec<(String, String)>,
    pub children: Vec<Node>,
}

/// A compiled top-level rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub id: String,
    pub source: String,
    pub label: String,
    pub mime: Option<String>,
    pub description: Option<String>,
    pub confidence: u8,
    pub strength: i32,
    /// Ask for the bytes of the top-level test when they are missing (unidentified files only).
    pub fetch: bool,
    /// Bytes from the start / end of the file the rule reads (for planners and wants).
    pub head: u64,
    pub tail: u64,
    pub root: Node,
}

/// Decodes `\xNN`, `\0`, `\n`, `\r`, `\t`, `\\` and `\'` in a literal string value.
fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let e = *b.get(i + 1).ok_or("dangling backslash")?;
        i += 2;
        match e {
            b'x' => {
                let hx = s.get(i..i + 2).ok_or("short \\x escape")?;
                out.push(u8::from_str_radix(hx, 16).map_err(|_| format!("bad \\x escape {hx}"))?);
                i += 2;
            }
            b'0' => out.push(0),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            other => return Err(format!("unknown escape \\{}", char::from(other))),
        }
    }
    Ok(out)
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        return Err("odd number of hex digits".into());
    }
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            let t = std::str::from_utf8(p).map_err(|_| "bad hex".to_string())?;
            u8::from_str_radix(t, 16).map_err(|_| format!("bad hex {t}"))
        })
        .collect()
}

fn parse_num(s: &str) -> Result<Operand, String> {
    let s = s.trim();
    if s == "$size" {
        return Ok(Operand::Size);
    }
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(&h.replace('_', ""), 16)
    } else {
        s.replace('_', "").parse::<u64>()
    };
    v.map(Operand::Lit).map_err(|_| format!("bad number {s:?}"))
}

fn lit(o: Operand) -> Result<u64, String> {
    match o {
        Operand::Lit(v) => Ok(v),
        Operand::Size => Err("$size is not allowed here".into()),
    }
}

/// `x`, `=N`, `!=N`, `<N`, `<=N`, `>N`, `>=N`, `&N`, `^N`, `A..B`, `A..=B`, alternatives
/// separated by `|` (`1|2|0x10`). A bare number means `=N`.
fn parse_cmps(s: &str) -> Result<Vec<Cmp>, String> {
    s.split('|')
        .map(|t| {
            let t = t.trim();
            if t == "x" || t.is_empty() {
                return Ok(Cmp::Any);
            }
            if let Some((a, b)) = t.split_once("..") {
                let (b, incl) = match b.strip_prefix('=') {
                    Some(b) => (b, true),
                    None => (b, false),
                };
                return Ok(Cmp::Range(lit(parse_num(a)?)?, lit(parse_num(b)?)?, incl));
            }
            for (p, f) in [
                ("!=", Cmp::Ne as fn(Operand) -> Cmp),
                ("<=", Cmp::Le),
                (">=", Cmp::Ge),
                ("==", Cmp::Eq),
                ("=", Cmp::Eq),
                ("<", Cmp::Lt),
                (">", Cmp::Gt),
            ] {
                if let Some(r) = t.strip_prefix(p) {
                    return Ok(f(parse_num(r)?));
                }
            }
            if let Some(r) = t.strip_prefix('&') {
                return Ok(Cmp::AllSet(lit(parse_num(r)?)?));
            }
            if let Some(r) = t.strip_prefix('^') {
                return Ok(Cmp::NoneSet(lit(parse_num(r)?)?));
            }
            Ok(Cmp::Eq(parse_num(t)?))
        })
        .collect()
}

fn parse_class(s: &str) -> Result<Class, String> {
    Ok(match s {
        "printable" => Class::Printable,
        "alnum" => Class::Alnum,
        "upper_alnum" => Class::UpperAlnum,
        "digit" => Class::Digit,
        "hex" => Class::Hex,
        "graphic" => Class::Graphic,
        "whitespace" => Class::Whitespace,
        _ => return Err(format!("unknown class {s:?}")),
    })
}

fn byte_values(n: &mut RawNode) -> Result<Vec<Vec<u8>>, String> {
    let mut alts = Vec::new();
    if let Some(v) = n.value.take() {
        for s in v.into_vec() {
            alts.push(unescape(&s)?);
        }
    }
    if let Some(v) = n.hex.take() {
        for s in v.into_vec() {
            alts.push(unhex(&s)?);
        }
    }
    if alts.len() > MAX_ALTS {
        return Err(format!("at most {MAX_ALTS} alternatives per test"));
    }
    if alts.is_empty()
        || alts
            .iter()
            .any(|a| a.is_empty() || a.len() as u64 > MAX_LEN)
    {
        return Err("a byte test needs non-empty `value` or `hex` (at most 64 KiB)".into());
    }
    Ok(alts)
}

fn compile_node(mut n: RawNode, depth: usize, top: bool) -> Result<Node, String> {
    if depth > MAX_DEPTH {
        return Err(format!("rules nest deeper than {MAX_DEPTH} levels"));
    }
    if !top
        && (n.id.is_some()
            || n.mime.is_some()
            || n.description.is_some()
            || n.strength.is_some()
            || n.fetch.is_some()
            || n.head.is_some()
            || n.tail.is_some())
    {
        return Err("id/mime/description/strength/fetch/head/tail are top-level keys".into());
    }
    if top && n.optional {
        return Err("a top-level test cannot be optional".into());
    }
    if n.confidence.is_some_and(|c| c > 100) {
        return Err("confidence is 0-100".into());
    }
    let offset = match n.offset.take() {
        None => Offset::Start(0),
        Some(RawOffset::Abs(v)) if v < 0 => Offset::End(v.unsigned_abs()),
        Some(RawOffset::Abs(v)) => Offset::Start(v.unsigned_abs()),
        Some(RawOffset::Indirect(i)) => Offset::Indirect {
            at: i.at,
            kind: NumKind::parse(&i.kind)
                .ok_or_else(|| format!("unknown indirect type {:?}", i.kind))?,
            add: i.add,
            mul: i.mul,
        },
    };
    if n.relative && !matches!(offset, Offset::Start(_)) {
        return Err("relative offsets must be non-negative integers".into());
    }
    if top && n.relative {
        return Err("a top-level test cannot be relative".into());
    }
    let test = if let Some(kind) = NumKind::parse(&n.kind) {
        let cmps = parse_cmps(n.test.as_deref().unwrap_or("x"))?;
        Test::Num {
            kind,
            mask: n.mask,
            shift: n.shift.unwrap_or(0).min(63),
            cmps,
        }
    } else {
        match n.kind.as_str() {
            "string" | "bytes" => Test::Bytes {
                alts: byte_values(&mut n)?,
                nocase: n.nocase,
            },
            "search" => {
                let range = n.range.ok_or("search needs `range`")?;
                if range > MAX_SEARCH {
                    return Err("search range is at most 1 MiB".into());
                }
                Test::Search {
                    alts: byte_values(&mut n)?,
                    range,
                }
            }
            "ascii" | "cstring" | "utf16le" | "utf16be" => {
                let len = n.len.ok_or("text types need `len`")?;
                if len == 0 || len > 4096 {
                    return Err("text len is 1-4096".into());
                }
                let kind = match n.kind.as_str() {
                    "ascii" => TextKind::Ascii,
                    "cstring" => TextKind::Cstring,
                    "utf16le" => TextKind::Utf16Le,
                    _ => TextKind::Utf16Be,
                };
                let class = n.class.as_deref().map(parse_class).transpose()?;
                Test::Text { kind, len, class }
            }
            other => return Err(format!("unknown type {other:?}")),
        }
    };
    let children = n
        .then
        .into_iter()
        .map(|c| compile_node(c, depth + 1, false))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Node {
        offset,
        relative: n.relative,
        test,
        field: n.field,
        optional: n.optional,
        label: if top { None } else { n.label },
        confidence: if top { None } else { n.confidence },
        note: n.note,
        set: n.set.into_iter().collect(),
        children,
    })
}

/// Bytes the node tree reads from the start / end of the file (fixed offsets only).
fn needs(n: &Node, parent_end: u64, head: &mut u64, tail: &mut u64) {
    let width = match &n.test {
        Test::Num { kind, .. } => kind.width() as u64,
        Test::Bytes { alts, .. } => alts.iter().map(|a| a.len() as u64).max().unwrap_or(0),
        Test::Search { alts, range } => {
            range + alts.iter().map(|a| a.len() as u64).max().unwrap_or(0)
        }
        Test::Text { len, .. } => *len,
    };
    let end = match n.offset {
        Offset::Start(o) if n.relative => Some(parent_end.saturating_add(o).saturating_add(width)),
        Offset::Start(o) => Some(o.saturating_add(width)),
        Offset::End(o) => {
            *tail = (*tail).max(o);
            None
        }
        Offset::Indirect { at, kind, .. } => {
            if at >= 0 {
                *head = (*head).max(at.unsigned_abs().saturating_add(kind.width() as u64));
            } else {
                *tail = (*tail).max(at.unsigned_abs());
            }
            None
        }
    };
    if let Some(e) = end {
        *head = (*head).max(e);
    }
    for c in &n.children {
        needs(c, end.unwrap_or(parent_end), head, tail);
    }
}

/// Counts the tests of a rule tree and the bytes its searches scan.
fn cost(n: &Node, nodes: &mut usize, search: &mut u64) {
    *nodes = nodes.saturating_add(1);
    if let Test::Search { alts, range } = &n.test {
        let longest = alts.iter().map(|a| a.len() as u64).max().unwrap_or(0);
        *search = search.saturating_add((alts.len() as u64).saturating_mul(range + longest));
    }
    for c in &n.children {
        cost(c, nodes, search);
    }
}

/// Compiles one TOML signature file. `source` names it in errors and rule ids.
pub(crate) fn compile(source: &str, text: &str) -> Result<Vec<Rule>, SigError> {
    let err = |rule: &str, message: String| SigError {
        source: source.to_owned(),
        rule: rule.to_owned(),
        message,
    };
    let raw: RawFile = toml::from_str(text).map_err(|e| err("-", e.to_string()))?;
    if raw.rule.len() > MAX_RULES {
        return Err(err("-", format!("at most {MAX_RULES} rules per file")));
    }
    let mut out = Vec::with_capacity(raw.rule.len());
    let (mut nodes, mut search_cost) = (0usize, 0u64);
    for (i, r) in raw.rule.into_iter().enumerate() {
        let id = r.id.clone().unwrap_or_else(|| format!("{source}#{i}"));
        let label = r
            .label
            .clone()
            .ok_or_else(|| err(&id, "missing label".into()))?;
        if label.is_empty()
            || !label
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_')
        {
            return Err(err(
                &id,
                format!("label {label:?} must be lowercase dotted ([a-z0-9._])"),
            ));
        }
        let confidence = r
            .confidence
            .ok_or_else(|| err(&id, "missing confidence".into()))?;
        let (mime, description, strength, fetch, head_o, tail_o) = (
            r.mime.clone(),
            r.description.clone(),
            r.strength,
            r.fetch,
            r.head,
            r.tail,
        );
        let root = compile_node(r, 0, true).map_err(|m| err(&id, m))?;
        cost(&root, &mut nodes, &mut search_cost);
        if nodes > MAX_NODES {
            return Err(err(&id, format!("at most {MAX_NODES} tests per file")));
        }
        if search_cost > MAX_SEARCH_COST {
            return Err(err(
                &id,
                "search tests too costly: alternatives x window over 64 MiB per file".into(),
            ));
        }
        let (mut head, mut tail) = (0, 0);
        needs(&root, 0, &mut head, &mut tail);
        out.push(Rule {
            id,
            source: source.to_owned(),
            label,
            mime,
            description,
            confidence: confidence.min(100),
            strength: strength.unwrap_or(i32::from(confidence)),
            fetch: fetch.unwrap_or(true),
            head: head_o.unwrap_or(head),
            tail: tail_o.unwrap_or(tail),
            root,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_numbers() {
        assert_eq!(unescape(r"\x7FFIH\0").unwrap(), b"\x7FFIH\0");
        assert!(unescape(r"\q").is_err());
        assert_eq!(unhex("1A45 DFA3").unwrap(), vec![0x1A, 0x45, 0xDF, 0xA3]);
        assert_eq!(
            parse_cmps("1|0x10").unwrap(),
            vec![Cmp::Eq(Operand::Lit(1)), Cmp::Eq(Operand::Lit(16))]
        );
        assert_eq!(
            parse_cmps("3800..5000").unwrap(),
            vec![Cmp::Range(3800, 5000, false)]
        );
        assert_eq!(parse_cmps("<=6").unwrap(), vec![Cmp::Le(Operand::Lit(6))]);
        assert_eq!(parse_cmps("=$size").unwrap(), vec![Cmp::Eq(Operand::Size)]);
        assert_eq!(parse_cmps("&0x40").unwrap(), vec![Cmp::AllSet(0x40)]);
    }

    #[test]
    fn rejects_bad_rules() {
        let bad = [
            "[[rule]]\nlabel='a'\nconfidence=1\ntype='nope'",
            "[[rule]]\nlabel='A'\nconfidence=1\ntype='u8'",
            "[[rule]]\nlabel='a'\ntype='u8'",
            "[[rule]]\nlabel='a'\nconfidence=1\ntype='string'",
            "[[rule]]\nlabel='a'\nconfidence=1\ntype='u8'\n[[rule.then]]\ntype='u8'\nid='x'",
            "[[rule]]\nlabel='a'\nconfidence=1\ntype='u8'\nunknown_key=1",
        ];
        for b in bad {
            assert!(compile("t", b).is_err(), "{b}");
        }
    }

    /// Review finding: a user file had no cap on rules, tests or search alternatives, so
    /// 10 000 alternatives over a 1 MiB window made every pass take 0.8 s.
    #[test]
    fn costly_user_files_are_refused() {
        let alts = |n: usize| {
            (0..n)
                .map(|i| format!("'ZZ{i:06}'"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let search = |n: usize| {
            format!(
                "[[rule]]\nlabel='a'\nconfidence=90\ntype='search'\nrange=1048576\nvalue=[{}]",
                alts(n)
            )
        };
        assert!(compile("t", &search(32)).is_ok());
        assert!(compile("t", &search(1000)).is_err());
        let many: String = (0..80).map(|_| search(32)).collect::<Vec<_>>().join("\n");
        assert!(compile("t", &many).is_err(), "total search cost");
        let rules: String = (0..5000)
            .map(|_| "[[rule]]\nlabel='a'\nconfidence=1\ntype='u8'\n")
            .collect();
        assert!(compile("t", &rules).is_err(), "rule count");
    }

    #[test]
    fn computes_head_and_tail_needs() {
        let r = compile(
            "t",
            "[[rule]]\nlabel='a'\nconfidence=90\noffset=0x100\ntype='string'\nvalue='NCSD'\n[[rule.then]]\noffset=-512\ntype='u8'",
        )
        .unwrap();
        assert_eq!((r[0].head, r[0].tail), (0x104, 512));
    }
}
