//! NIP-01 filters as served by `/relay`: parsed leniently from JSON, matched in memory for live
//! events and translated to SQL by `PgStore` for stored ones.

use std::collections::BTreeMap;

use magnetar_core::model::{RawEvent, is_hex32};
use serde_json::Value;

/// Hard cap on stored events returned per filter (ARCHITECTURE.md §6).
pub const MAX_LIMIT: usize = 500;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub ids: Option<Vec<String>>,
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u16>>,
    /// Single-letter tag filters (`#e`, `#x`, `#t`, ...), matched against the first tag value.
    pub tags: BTreeMap<char, Vec<String>>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<usize>,
}

impl Filter {
    /// Parse one filter object. Errors carry a NIP-01 prefixed reason.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "invalid: filter is not an object".to_string())?;
        let mut f = Filter::default();
        for (key, val) in obj {
            match key.as_str() {
                "ids" => f.ids = Some(hex_list(key, val)?),
                "authors" => f.authors = Some(hex_list(key, val)?),
                "kinds" => {
                    let arr = val
                        .as_array()
                        .ok_or_else(|| "invalid: kinds must be an array".to_string())?;
                    let kinds = arr
                        .iter()
                        .map(|k| {
                            k.as_u64()
                                .and_then(|k| u16::try_from(k).ok())
                                .ok_or_else(|| "invalid: bad kind".to_string())
                        })
                        .collect::<Result<_, _>>()?;
                    f.kinds = Some(kinds);
                }
                "since" => f.since = Some(int(key, val)?),
                "until" => f.until = Some(int(key, val)?),
                "limit" => {
                    let n = val.as_u64().ok_or_else(|| {
                        "invalid: limit must be a non-negative integer".to_string()
                    })?;
                    f.limit = Some(n.min(MAX_LIMIT as u64) as usize);
                }
                k if k.len() == 2 && k.starts_with('#') => {
                    let letter = k.chars().nth(1).filter(char::is_ascii_alphabetic);
                    let Some(letter) = letter else {
                        return Err(format!("unsupported: filter item {k}"));
                    };
                    let arr = val
                        .as_array()
                        .ok_or_else(|| format!("invalid: {k} must be an array"))?;
                    let vals = arr
                        .iter()
                        .map(|s| {
                            s.as_str()
                                .map(str::to_string)
                                .ok_or_else(|| format!("invalid: {k} values must be strings"))
                        })
                        .collect::<Result<_, _>>()?;
                    f.tags.insert(letter, vals);
                }
                other => return Err(format!("unsupported: filter item {other}")),
            }
        }
        Ok(f)
    }

    /// Effective stored-event limit (absent = cap).
    pub fn effective_limit(&self) -> usize {
        self.limit.unwrap_or(MAX_LIMIT).min(MAX_LIMIT)
    }

    /// NIP-77 sessions only support kinds/authors/since/until/ids (negentropy.md §5).
    pub fn neg_compatible(&self) -> bool {
        self.tags.is_empty() && self.limit.is_none()
    }

    pub fn matches(&self, ev: &RawEvent) -> bool {
        if let Some(ids) = &self.ids
            && !ids.contains(&ev.id)
        {
            return false;
        }
        if let Some(authors) = &self.authors
            && !authors.contains(&ev.pubkey)
        {
            return false;
        }
        if let Some(kinds) = &self.kinds
            && !kinds.contains(&ev.kind)
        {
            return false;
        }
        if self.since.is_some_and(|s| ev.created_at < s)
            || self.until.is_some_and(|u| ev.created_at > u)
        {
            return false;
        }
        self.tags.iter().all(|(letter, wanted)| {
            ev.tags.iter().any(|t| {
                let mut name = [0u8; 4];
                t.first().map(String::as_str) == Some(letter.encode_utf8(&mut name))
                    && t.get(1).is_some_and(|v| wanted.contains(v))
            })
        })
    }
}

fn hex_list(key: &str, val: &Value) -> Result<Vec<String>, String> {
    let arr = val
        .as_array()
        .ok_or_else(|| format!("invalid: {key} must be an array"))?;
    arr.iter()
        .map(|s| match s.as_str() {
            Some(h) if is_hex32(h) => Ok(h.to_string()),
            _ => Err(format!("invalid: {key} must be 64-char lowercase hex")),
        })
        .collect()
}

fn int(key: &str, val: &Value) -> Result<i64, String> {
    val.as_i64()
        .ok_or_else(|| format!("invalid: {key} must be an integer"))
}

/// Single-letter tags and their first value, deduplicated (what relays index, nostr-nips.md §1.3).
pub fn indexed_tags(ev: &RawEvent) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = ev
        .tags
        .iter()
        .filter_map(|t| {
            let name = t.first()?;
            let value = t.get(1)?;
            (name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() && !value.contains('\0'))
                .then(|| (name.clone(), value.clone()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: u16, tags: Vec<Vec<&str>>) -> RawEvent {
        RawEvent {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 100,
            kind,
            tags: tags
                .into_iter()
                .map(|t| t.into_iter().map(String::from).collect())
                .collect(),
            content: String::new(),
            sig: String::new(),
        }
    }

    #[test]
    fn parse_and_match() {
        let f = Filter::from_json(&json!({"kinds":[2003],"#x":["abc"],"since":50,"limit":9999}))
            .unwrap();
        assert_eq!(f.effective_limit(), MAX_LIMIT);
        assert!(!f.neg_compatible());
        assert!(f.matches(&ev(2003, vec![vec!["x", "abc"]])));
        assert!(!f.matches(&ev(2003, vec![vec!["x", "zzz"]])));
        assert!(!f.matches(&ev(2004, vec![vec!["x", "abc"]])));
        let f = Filter::from_json(&json!({"until": 99})).unwrap();
        assert!(!f.matches(&ev(1, vec![])));
        // empty list matches nothing
        let f = Filter::from_json(&json!({"kinds": []})).unwrap();
        assert!(!f.matches(&ev(1, vec![])));
    }

    #[test]
    fn rejects_bad_filters() {
        assert!(
            Filter::from_json(&json!({"search":"x"}))
                .unwrap_err()
                .starts_with("unsupported:")
        );
        assert!(
            Filter::from_json(&json!({"ids":["ABC"]}))
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert!(Filter::from_json(&json!({"kinds":[70000]})).is_err());
        assert!(Filter::from_json(&json!([])).is_err());
        assert!(
            Filter::from_json(&json!({"authors":["b".repeat(64)]}))
                .unwrap()
                .neg_compatible()
        );
    }

    #[test]
    fn tag_index() {
        let e = ev(
            2003,
            vec![
                vec!["x", "1"],
                vec!["x", "1"],
                vec!["title", "t"],
                vec!["t"],
            ],
        );
        assert_eq!(indexed_tags(&e), vec![("x".into(), "1".into())]);
    }
}
