//! Free text → Tantivy query (D43): every word must match (title, file names or description);
//! BM25 with title > files > description, the last word also as a prefix (≥ 3 chars), typo
//! tolerance on longer words, and a flat bonus when every word is in the title so title hits
//! always rank above file-only ones.

use magnetar_core::query::MIN_PREFIX_CHARS;
use tantivy::Term;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, FuzzyTermQuery, Occur, Query, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::tokenizer::TokenStream;

use super::schema::{Fields, analyzer};

/// At most this many words of a query are used (as `core::query::tokens`).
pub const MAX_TOKENS: usize = 16;
/// Added to the score of documents whose title holds every word.
pub const TITLE_BONUS: f32 = 10_000.0;

const BOOST_TITLE: f32 = 3.0;
const BOOST_FILES: f32 = 1.5;
const BOOST_DESCRIPTION: f32 = 0.6;
/// Prefix and fuzzy matches score a constant (times these), below a real BM25 term hit.
const BOOST_PREFIX: f32 = 1.0;
const BOOST_FUZZY: f32 = 0.5;
/// A word found as is in at least this many titles is taken as typed: no fuzzy expansion
/// (`complete`, `french`: expanding them costs tens of ms at 3 M documents and only adds noise).
pub const FUZZY_MAX_DF: u64 = 50;

/// Words as the index sees them: split on non-alphanumerics, lowercased, accents folded.
pub fn tokens(text: &str) -> Vec<String> {
    let mut a = analyzer();
    let mut stream = a.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        if out.len() == MAX_TOKENS {
            break;
        }
        out.push(stream.token().text.clone());
    }
    out
}

/// Edit distance allowed for a word: none below 5 chars, 1 from 5, 2 from 8. Never for words
/// with digits: `s01e02` must not find `s01e01`, nor `2019` find `2018`.
pub fn fuzzy_distance(word: &str) -> u8 {
    if word.chars().any(|c| c.is_ascii_digit()) {
        return 0;
    }
    match word.chars().count() {
        n if n >= 8 => 2,
        n if n >= 5 => 1,
        _ => 0,
    }
}

fn prefix_ok(word: &str, last: bool) -> bool {
    last && word.chars().count() >= MIN_PREFIX_CHARS
}

fn term(field: Field, word: &str) -> Term {
    Term::from_field_text(field, word)
}

fn boosted(q: impl Query, boost: f32) -> Box<dyn Query> {
    Box::new(BoostQuery::new(Box::new(q), boost))
}

/// Ways one word may match one field: exact (BM25), prefix, and fuzzy when `fuzzy`.
fn variants(
    field: Field,
    word: &str,
    last: bool,
    fuzzy: bool,
    boost: f32,
    out: &mut Vec<Box<dyn Query>>,
) {
    out.push(boosted(
        TermQuery::new(term(field, word), IndexRecordOption::WithFreqs),
        boost,
    ));
    if prefix_ok(word, last) {
        out.push(boosted(
            FuzzyTermQuery::new_prefix(term(field, word), 0, true),
            BOOST_PREFIX * boost / BOOST_TITLE,
        ));
    }
    let d = fuzzy_distance(word);
    if fuzzy && d > 0 {
        let t = term(field, word);
        let q = if last {
            FuzzyTermQuery::new_prefix(t, d, true)
        } else {
            FuzzyTermQuery::new(t, d, true)
        };
        out.push(boosted(q, BOOST_FUZZY * boost / BOOST_TITLE));
    }
}

fn union(qs: Vec<Box<dyn Query>>) -> Box<dyn Query> {
    Box::new(BooleanQuery::new(
        qs.into_iter().map(|q| (Occur::Should, q)).collect(),
    ))
}

/// The query for analysed words; `AllQuery` when there are none (browse). `title_df` gives the
/// number of documents whose title holds a word as is (fuzzy matching is skipped for common
/// words, see `FUZZY_MAX_DF`).
pub fn build(f: &Fields, words: &[String], title_df: &dyn Fn(&Term) -> u64) -> Box<dyn Query> {
    if words.is_empty() {
        return Box::new(AllQuery);
    }
    let n = words.len();
    let mut must = Vec::with_capacity(n + 1);
    let mut title_all = Vec::with_capacity(n);
    for (i, w) in words.iter().enumerate() {
        let last = i + 1 == n;
        let fuzzy = fuzzy_distance(w) > 0 && title_df(&term(f.title, w)) < FUZZY_MAX_DF;
        let mut title = Vec::new();
        variants(f.title, w, last, fuzzy, BOOST_TITLE, &mut title);
        let mut any = title.iter().map(|q| q.box_clone()).collect::<Vec<_>>();
        // File names: exact and prefix only; typos are matched against titles.
        variants(f.files, w, last, false, BOOST_FILES, &mut any);
        // Descriptions are long and mostly boilerplate: exact words (and a long last-word
        // prefix) only, no fuzzy expansion over their large term dictionary.
        any.push(boosted(
            TermQuery::new(term(f.description, w), IndexRecordOption::WithFreqs),
            BOOST_DESCRIPTION,
        ));
        if last && w.chars().count() >= 4 {
            any.push(boosted(
                FuzzyTermQuery::new_prefix(term(f.description, w), 0, true),
                BOOST_PREFIX * BOOST_DESCRIPTION / BOOST_TITLE,
            ));
        }
        must.push((Occur::Must, union(any)));
        title_all.push((Occur::Must, union(title)));
    }
    let bonus: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(title_all)),
        TITLE_BONUS,
    ));
    must.push((Occur::Should, bonus));
    Box::new(BooleanQuery::new(must))
}

/// Documents where some word matches a file name (as `build` matches files).
pub fn files_any(f: &Fields, words: &[String]) -> BooleanQuery {
    let n = words.len();
    let mut any = Vec::new();
    for (i, w) in words.iter().enumerate() {
        variants(f.files, w, i + 1 == n, false, BOOST_FILES, &mut any);
    }
    BooleanQuery::new(any.into_iter().map(|q| (Occur::Should, q)).collect())
}

/// Optimal string alignment distance (adjacent transpositions cost 1, like the index's fuzzy
/// queries), or `max + 1` once it is certainly above `max`.
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let w = b.len() + 1;
    let mut d = vec![0usize; (a.len() + 1) * w];
    for (j, v) in d.iter_mut().take(w).enumerate() {
        *v = j;
    }
    for i in 1..=a.len() {
        d[i * w] = i;
        let mut row_min = usize::MAX;
        for j in 1..w {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[(i - 1) * w + j] + 1)
                .min(d[i * w + j - 1] + 1)
                .min(d[(i - 1) * w + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[(i - 2) * w + j - 2] + 1);
            }
            d[i * w + j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return max + 1;
        }
    }
    d[a.len() * w + b.len()]
}

/// How one analysed word of the query matches one analysed word of a text, the way `build`
/// matches it: 3 exact, 2 prefix, 1 fuzzy, 0 not at all.
pub fn word_score(query: &str, word: &str, last: bool) -> u8 {
    if query == word {
        3
    } else if prefix_ok(query, last) && word.starts_with(query) {
        2
    } else if fuzzy_match(query, word, last) {
        1
    } else {
        0
    }
}

pub fn word_matches(query: &str, word: &str, last: bool) -> bool {
    word_score(query, word, last) > 0
}

fn fuzzy_match(query: &str, word: &str, last: bool) -> bool {
    let d = usize::from(fuzzy_distance(query));
    if d == 0 {
        return false;
    }
    if last {
        // Prefix within distance d: some prefix of `word` is close enough.
        let chars: Vec<char> = word.chars().collect();
        let n = query.chars().count();
        (n.saturating_sub(d)..=(n + d).min(chars.len())).any(|k| {
            let p: String = chars[..k].iter().collect();
            edit_distance(query, &p, d) <= d
        })
    } else {
        edit_distance(query, word, d) <= d
    }
}

/// Every query word found among `text`'s words.
pub fn all_match(words: &[String], text: &str) -> bool {
    let have = tokens_unbounded(text);
    let n = words.len();
    words.iter().enumerate().all(|(i, q)| {
        let last = i + 1 == n;
        have.iter().any(|w| word_matches(q, w, last))
    })
}

fn tokens_unbounded(text: &str) -> Vec<String> {
    let mut a = analyzer();
    let mut stream = a.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

/// The file path matching the query best (exact > prefix > fuzzy per word, summed; ties: the
/// shorter path), if any word matches it.
pub fn best_file<'a>(words: &[String], paths: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let n = words.len();
    let mut best: Option<(u32, usize, &str)> = None;
    for p in paths {
        let have = tokens_unbounded(p);
        let hits: u32 = words
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let s = have.iter().map(|w| word_score(q, w, i + 1 == n)).max();
                u32::from(s.unwrap_or(0))
            })
            .sum();
        if hits == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((h, len, _)) => hits > h || (hits == h && p.len() < len),
        };
        if better {
            best = Some((hits, p.len(), p));
        }
    }
    best.map(|(_, _, p)| p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_folds_accents_and_case() {
        assert_eq!(
            tokens("Été.2024 FRENCH 1080p"),
            vec!["ete", "2024", "french", "1080p"]
        );
        assert_eq!(
            tokens("Le Seigneur des Anneaux"),
            vec!["le", "seigneur", "des", "anneaux"]
        );
        assert_eq!(
            tokens("[GRP] Show_-_01 (x265)"),
            vec!["grp", "show", "01", "x265"]
        );
        assert!(tokens(" .. ").is_empty());
        assert_eq!(tokens(&"a ".repeat(40)).len(), MAX_TOKENS);
    }

    #[test]
    fn distances() {
        assert_eq!(fuzzy_distance("dune"), 0);
        assert_eq!(fuzzy_distance("ubunt"), 1);
        assert_eq!(fuzzy_distance("interstellar"), 2);
        assert_eq!(edit_distance("ubunto", "ubuntu", 1), 1);
        assert_eq!(edit_distance("ubnutu", "ubuntu", 1), 1, "transposition");
        assert_eq!(edit_distance("abcdef", "uvwxyz", 2), 3);
        assert_eq!(edit_distance("", "abc", 5), 3);
    }

    #[test]
    fn word_matching() {
        assert!(word_matches("ubuntu", "ubuntu", false));
        assert!(word_matches("ubunto", "ubuntu", false));
        assert!(
            !word_matches("dune", "dine", false),
            "no fuzzy below 5 chars"
        );
        assert!(
            !word_matches("s01e02", "s01e01", false),
            "no fuzzy with digits"
        );
        assert!(word_matches("bun", "bunny", true));
        assert!(
            !word_matches("bu", "bunny", true),
            "no prefix below 3 chars"
        );
        assert!(
            !word_matches("bun", "bunny", false),
            "prefix only on the last word"
        );
        assert!(word_matches("interstelar", "interstellar", true));
        assert!(word_matches("seigne", "seigneur", true));
        assert!(all_match(&tokens("big buck bun"), "Big.Buck.Bunny.2008"));
        assert!(!all_match(&tokens("big buck bun"), "Big.Buck.2008"));
    }

    #[test]
    fn best_file_prefers_most_words_then_shorter() {
        let w = tokens("s01e02 1080p");
        let files = [
            "Show/Show.S01E01.1080p.mkv",
            "Show/Show.S01E02.1080p.mkv",
            "Show/Extras/Show.S01E02.1080p.Behind.The.Scenes.mkv",
        ];
        assert_eq!(
            best_file(&w, files).as_deref(),
            Some("Show/Show.S01E02.1080p.mkv")
        );
        assert_eq!(best_file(&tokens("zzz"), files), None);
    }
}
