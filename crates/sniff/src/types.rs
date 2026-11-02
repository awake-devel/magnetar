//! Public result types: [`Outcome`], [`Detection`], [`Value`], [`Flags`], [`Caps`].

use std::borrow::Cow;

use serde::Serialize;

use crate::formats::console::GameInfo;
use crate::sparse::Range;
use crate::video::VideoInfo;

/// A typed field value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    U64(u64),
    Str(String),
    List(Vec<String>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::U64(v) => Some(*v),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::U64(v) => write!(f, "{v}"),
            Value::Str(s) => f.write_str(s),
            Value::List(l) => f.write_str(&l.join(",")),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Str(s.to_owned())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}
impl From<&String> for Value {
    fn from(s: &String) -> Value {
        Value::Str(s.clone())
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Value {
        Value::U64(v)
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Value {
        Value::U64(u64::from(v))
    }
}
impl From<u16> for Value {
    fn from(v: u16) -> Value {
        Value::U64(u64::from(v))
    }
}
impl From<u8> for Value {
    fn from(v: u8) -> Value {
        Value::U64(u64::from(v))
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Value {
        Value::U64(v as u64)
    }
}
impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Value {
        Value::List(v)
    }
}

/// One extracted detail (`key` is a stable snake_case name).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Field {
    pub key: Cow<'static, str>,
    pub value: Value,
}

/// Flags raised next to the type; none of them changes the label.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Flags {
    /// The name has a known extension that the detected type is not normally seen with.
    pub extension_mismatch: bool,
    /// Executable content (or an executable double extension) behind a media/document name, or
    /// an archive listing executables.
    pub suspicious_executable: bool,
    /// The verdict read bytes inserted with [`crate::SparseFile::insert_unverified`].
    pub unverified: bool,
    /// The parser wanted more ranges than the round/byte budget allowed: fields may be missing.
    pub incomplete: bool,
}

/// A losing (or the winning) candidate, for `--explain` and tie analysis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub label: String,
    pub confidence: u8,
    /// Rust check name or signature rule id.
    pub source: String,
}

/// A file type verdict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Detection {
    /// Stable dotted id: `video.mkv`, `game.ps5.pkg`, `archive.zip`, `text.nfo`.
    pub label: String,
    /// MIME-ish type (`video/x-matroska`, `application/x-ps5-pkg`).
    pub mime: String,
    /// Human description.
    pub description: String,
    /// 0-100: 95+ = signature plus a passing structural check (or a magic of 8 bytes and more),
    /// 80+ = signature with partial checks, at most 79 for a magic shorter than 8 bytes without
    /// a passing structural check. Store only 80 and above as facts.
    pub confidence: u8,
    pub fields: Vec<Field>,
    /// Console package / image details (content ID, title ID, title, version, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game: Option<GameInfo>,
    /// Video recon (codec, resolution, HDR, tracks, ...) for video containers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoInfo>,
    pub flags: Flags,
    /// Extensions this type is normally seen with (reported only; never used to decide).
    pub extensions: Vec<String>,
    /// Every candidate considered, best first.
    pub candidates: Vec<Candidate>,
}

impl Detection {
    pub fn field(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|f| f.key == key).map(|f| &f.value)
    }
    pub fn field_str(&self, key: &str) -> Option<&str> {
        self.field(key).and_then(Value::as_str)
    }
}

/// Result of one detection pass over a [`crate::SparseFile`].
// One value per pass, moved to the caller: boxing the detection would only cost ergonomics.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Detected(Detection),
    /// The winning parser (or the fallback) wants these ranges before it can decide or finish.
    NeedMore(Vec<Range>),
    /// Nothing recognised the bytes.
    Unknown,
}

impl Outcome {
    pub fn detection(&self) -> Option<&Detection> {
        match self {
            Outcome::Detected(d) => Some(d),
            _ => None,
        }
    }
}

/// Budgets and caps for one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Caps {
    /// First read from the start of the file.
    pub head: u64,
    /// First read from the end of the file (0 = none; parsers ask for the tail when they need it).
    pub tail: u64,
    /// Follow-up rounds after the first read.
    pub rounds: u32,
    /// Total bytes the driver may fetch for one file.
    pub max_bytes: u64,
    /// Largest single range a parser may ask for.
    pub max_range: u64,
    /// Most ranges asked in one round.
    pub max_ranges: usize,
    /// Output cap when peeking inside gzip/zstd/zlib streams.
    pub inner_bytes: usize,
    /// Nesting limit for compressed-inside-compressed peeks.
    pub max_depth: u8,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            head: 16 * 1024,
            tail: 0,
            rounds: 3,
            max_bytes: 8 << 20,
            max_range: 4 << 20,
            max_ranges: 16,
            inner_bytes: 64 * 1024,
            max_depth: 2,
        }
    }
}

/// Highest confidence of a short magic (under 8 bytes) without a passing structural check:
/// shown, not stored (callers store 80 and above). Rust checks and signature rules alike.
pub(crate) const WEAK: u8 = 79;

/// Internal candidate produced by a Rust check or a signature rule.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hit {
    pub label: Cow<'static, str>,
    pub conf: u8,
    pub fields: Vec<Field>,
    /// Extra ranges that would refine or confirm this hit.
    pub need: Vec<Range>,
    pub game: Option<GameInfo>,
    pub source: Cow<'static, str>,
    /// Ordering among equal confidences (signature strength; Rust checks use 0).
    pub strength: i32,
    /// Overrides from user signatures for labels outside the registry.
    pub mime: Option<String>,
    pub description: Option<String>,
    /// The check read bytes inserted as unverified.
    pub unverified: bool,
}

impl Hit {
    pub fn new(label: &'static str, conf: u8) -> Hit {
        Hit {
            label: Cow::Borrowed(label),
            conf,
            ..Hit::default()
        }
    }
    pub fn d(mut self, key: &'static str, v: impl Into<Value>) -> Hit {
        self.fields.push(Field {
            key: Cow::Borrowed(key),
            value: v.into(),
        });
        self
    }
    pub fn push(&mut self, key: &'static str, v: impl Into<Value>) {
        self.fields.push(Field {
            key: Cow::Borrowed(key),
            value: v.into(),
        });
    }
    pub fn need(mut self, off: u64, len: u64) -> Hit {
        self.need.push(Range::new(off, len));
        self
    }
    pub fn has(&self, key: &str) -> bool {
        self.fields.iter().any(|f| f.key == key)
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn relabel(&mut self, label: &'static str) {
        self.label = Cow::Borrowed(label);
    }
}
