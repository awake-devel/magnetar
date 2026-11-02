//! Signature engine: rule trees in TOML, modelled on libmagic's magic(5) (continuation levels,
//! typed reads, indirect/relative/end-of-file offsets, strength ordering), evaluated against the
//! sparse bytes.
//!
//! Built-in rules are the files under `data/signatures/` (embedded at compile time). A user file
//! can be added at runtime with [`Signatures::with_user`]; the library never reads files itself.

use std::sync::OnceLock;

mod eval;
mod parse;

pub(crate) use eval::{RuleResult, eval_rule};
pub(crate) use parse::Rule;
pub use parse::SigError;

/// Embedded signature files (name, TOML text).
pub const BUILTIN_FILES: &[(&str, &str)] = &[
    (
        "media.toml",
        include_str!("../../data/signatures/media.toml"),
    ),
    (
        "image.toml",
        include_str!("../../data/signatures/image.toml"),
    ),
    ("font.toml", include_str!("../../data/signatures/font.toml")),
    ("doc.toml", include_str!("../../data/signatures/doc.toml")),
    (
        "archive.toml",
        include_str!("../../data/signatures/archive.toml"),
    ),
    ("exe.toml", include_str!("../../data/signatures/exe.toml")),
    ("disc.toml", include_str!("../../data/signatures/disc.toml")),
    ("game.toml", include_str!("../../data/signatures/game.toml")),
    ("misc.toml", include_str!("../../data/signatures/misc.toml")),
];

/// A compiled set of signature rules, sorted by strength (strongest first).
#[derive(Clone, Debug, Default)]
pub struct Signatures {
    rules: Vec<Rule>,
}

/// What a rule reads, for planners and `--explain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: String,
    pub source: String,
    pub label: String,
    pub confidence: u8,
    pub strength: i32,
    /// Bytes read from the start of the file (fixed offsets).
    pub head: u64,
    /// Bytes read from the end of the file.
    pub tail: u64,
}

impl Signatures {
    /// Compiles the embedded files.
    pub fn try_builtin() -> Result<Signatures, SigError> {
        let mut rules = Vec::new();
        for (name, text) in BUILTIN_FILES {
            rules.extend(parse::compile(name, text)?);
        }
        let mut s = Signatures { rules };
        s.sort();
        Ok(s)
    }

    /// The embedded rules, compiled once. Empty if they fail to compile (a test guards that).
    pub fn builtin() -> &'static Signatures {
        static B: OnceLock<Signatures> = OnceLock::new();
        B.get_or_init(|| Signatures::try_builtin().unwrap_or_default())
    }

    /// Compiles one TOML signature file.
    pub fn from_toml(source: &str, text: &str) -> Result<Signatures, SigError> {
        let mut s = Signatures {
            rules: parse::compile(source, text)?,
        };
        s.sort();
        Ok(s)
    }

    /// The built-in rules plus a user file (its rules sort in by strength like the others).
    pub fn with_user(source: &str, text: &str) -> Result<Signatures, SigError> {
        let mut s = Signatures::builtin().clone();
        s.rules.extend(parse::compile(source, text)?);
        s.sort();
        Ok(s)
    }

    fn sort(&mut self) {
        self.rules.sort_by_key(|r| std::cmp::Reverse(r.strength));
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(crate) fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn info(&self) -> Vec<RuleInfo> {
        self.rules
            .iter()
            .map(|r| RuleInfo {
                id: r.id.clone(),
                source: r.source.clone(),
                label: r.label.clone(),
                confidence: r.confidence,
                strength: r.strength,
                head: r.head,
                tail: r.tail,
            })
            .collect()
    }

    /// Largest fixed head any rule reads (a planner's first read covering every rule).
    pub fn max_head(&self) -> u64 {
        self.rules.iter().map(|r| r.head).max().unwrap_or(0)
    }

    /// Largest tail any rule reads.
    pub fn max_tail(&self) -> u64 {
        self.rules.iter().map(|r| r.tail).max().unwrap_or(0)
    }
}
