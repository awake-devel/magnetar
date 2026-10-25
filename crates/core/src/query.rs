//! Search query types and text sanitizers (ARCHITECTURE.md §5).

use serde::{Deserialize, Serialize};

use crate::model::{Category, TorrentSummary};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchQuery {
    pub text: String,
    pub categories: Vec<Category>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub min_seeders: Option<u32>,
    pub hide_dead: bool,
    /// None = use setting `trust.threshold`.
    pub min_trust: Option<u8>,
    pub sort: Sort,
    pub descending: bool,
    pub limit: u32,
    pub offset: u32,
    pub include_hidden: bool,
}

impl SearchQuery {
    /// Effective limit (default 50, max 200).
    pub fn effective_limit(&self) -> u32 {
        match self.limit {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        }
    }

    /// Relevance only makes sense with text; browse falls back to Date.
    pub fn effective_sort(&self) -> Sort {
        if self.sort == Sort::Relevance && self.text.trim().is_empty() {
            Sort::Date
        } else {
            self.sort
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Relevance,
    Date,
    Size,
    Seeders,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<TorrentSummary>,
    pub total: u64,
    /// "server" | "local"
    pub source: String,
    /// `total` is a lower bound ("1,000+"): the exact count was too expensive (D43). Older
    /// servers never send it.
    #[serde(default)]
    pub total_is_lower_bound: bool,
}

/// Split user text into safe word tokens (letters/digits in any script), lowercased.
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .take(16)
        .collect()
}

/// SQLite FTS5 MATCH expression: every token quoted, last one as prefix. None if no tokens.
pub fn fts5_match(text: &str) -> Option<String> {
    let toks = tokens(text);
    if toks.is_empty() {
        return None;
    }
    let n = toks.len();
    Some(
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                let q = format!("\"{}\"", t.replace('"', "\"\""));
                if i + 1 == n { format!("{q}*") } else { q }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Postgres `to_tsquery('simple', ..)` expression: tokens ANDed, last one prefix. None if no tokens.
/// Shortest last word that is still matched as a prefix (typed-as-you-go search).
pub const MIN_PREFIX_CHARS: usize = 3;

pub fn pg_tsquery(text: &str) -> Option<String> {
    let toks = tokens(text);
    if toks.is_empty() {
        return None;
    }
    let n = toks.len();
    Some(
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                let t = t.replace(['\'', '\\', ':', '&', '|', '!', '(', ')', '<', '>'], "");
                // A 1–2 letter prefix matches most of the index (`'o':*` took 42 s on 400k rows).
                if i + 1 == n && t.chars().count() >= MIN_PREFIX_CHARS {
                    format!("'{t}':*")
                } else {
                    format!("'{t}'")
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizers() {
        assert_eq!(
            fts5_match("Big.Buck \"bun"),
            Some("\"big\" \"buck\" \"bun\"*".into())
        );
        assert_eq!(fts5_match("  ..  "), None);
        assert_eq!(pg_tsquery("ubuntu 24"), Some("'ubuntu' & '24'".into()));
        assert_eq!(pg_tsquery("ubuntu 240"), Some("'ubuntu' & '240':*".into()));
        assert_eq!(pg_tsquery("o"), Some("'o'".into()));
        assert_eq!(pg_tsquery("onl"), Some("'onl':*".into()));
        assert_eq!(tokens("Été 1080p"), vec!["été", "1080p"]);
    }
}
