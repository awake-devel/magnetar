//! One-pass collectors over the matching documents: every filter of `SearchQuery` is applied
//! on fast columns, then the hit is counted and kept if it is in the top `offset + limit` for
//! the sort. Counting comes for free, so totals are exact on the index path.
//!
//! Facet counts (D44) are disjunctive: a facet's counts apply every filter except its own, so
//! the app can show how many results each other language/resolution/… would give. One pass
//! serves all requested facets: a document failing no attribute filter counts for every facet,
//! one failing exactly one filter counts only for that filter's facet.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::{ColumnValues, StrColumn};
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal, SegmentReader};

use super::schema::NONE;

/// Any-of filter on one keyword fast field (`f_language`, `f_resolution`, …; D44).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttrFilter {
    pub field: String,
    pub values: Vec<String>,
}

/// The per-query filters, resolved (threshold known, categories as a bit mask).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Bit `category_code` set = wanted; 0 = every category.
    pub categories: u64,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub min_trust: i64,
    pub hide_dead: bool,
    pub min_seeders: Option<i64>,
    pub media_id: Option<i64>,
    /// Rating × 10; unrated fails.
    pub min_rating: Option<i64>,
    /// Media year, else parsed year; unknown fails a bound.
    pub min_year: Option<i64>,
    pub max_year: Option<i64>,
    /// AND over fields, any value within one (D44).
    pub attrs: Vec<AttrFilter>,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Relevance,
    Date,
    Size,
    Seeders,
    Rating,
}

type I64Col = Arc<dyn ColumnValues<i64>>;
type U64Col = Arc<dyn ColumnValues<u64>>;

/// The fast columns of one segment the filters and sorts read.
struct Columns {
    created_at: I64Col,
    category: U64Col,
    total_size: I64Col,
    eligible_trust: I64Col,
    best_peers: I64Col,
    expires_at: I64Col,
    media_id: I64Col,
    media_rating: I64Col,
    year: I64Col,
    /// One per `Filter::attrs`, same order: the column and the term ordinals wanted in this
    /// segment (a value the segment never saw has no ordinal: nothing there matches it).
    attrs: Vec<(Option<StrColumn>, Vec<u64>)>,
}

impl Columns {
    fn load(seg: &SegmentReader, f: &Filter) -> tantivy::Result<Self> {
        let ff = seg.fast_fields();
        let mut attrs = Vec::with_capacity(f.attrs.len());
        for a in &f.attrs {
            let col = ff.str(&a.field)?;
            let mut wanted = Vec::new();
            if let Some(c) = &col {
                for v in &a.values {
                    if let Some(ord) = c.dictionary().term_ord(v)? {
                        wanted.push(ord);
                    }
                }
            }
            wanted.sort_unstable();
            attrs.push((col, wanted));
        }
        let i = |name: &str| -> tantivy::Result<I64Col> {
            Ok(ff.i64(name)?.first_or_default_col(NONE))
        };
        let u =
            |name: &str| -> tantivy::Result<U64Col> { Ok(ff.u64(name)?.first_or_default_col(0)) };
        Ok(Columns {
            created_at: i("created_at")?,
            category: u("category")?,
            total_size: i("total_size")?,
            eligible_trust: i("eligible_trust")?,
            best_peers: i("best_peers")?,
            expires_at: i("expires_at")?,
            media_id: i("media_id")?,
            media_rating: i("media_rating")?,
            year: i("year")?,
            attrs,
        })
    }

    /// Whether the document has one of the wanted values of attribute filter `k`.
    fn attr_passes(&self, k: usize, doc: DocId) -> bool {
        let (col, wanted) = &self.attrs[k];
        match col {
            Some(c) if !wanted.is_empty() => c
                .ords()
                .values_for_doc(doc)
                .any(|o| wanted.binary_search(&o).is_ok()),
            _ => false,
        }
    }

    /// Attribute filters the document fails: how many, and the first one (for facets).
    fn attr_failures(&self, doc: DocId) -> (usize, usize) {
        let mut n = 0;
        let mut first = 0;
        for k in 0..self.attrs.len() {
            if !self.attr_passes(k, doc) {
                if n == 0 {
                    first = k;
                }
                n += 1;
                if n > 1 {
                    break;
                }
            }
        }
        (n, first)
    }

    /// Every filter, attribute filters included.
    fn passes(&self, f: &Filter, doc: DocId) -> bool {
        self.base_passes(f, doc) && (0..self.attrs.len()).all(|k| self.attr_passes(k, doc))
    }

    /// ARCHITECTURE.md §5 semantics: unknown size fails a size bound; `hide_dead` drops only
    /// fresh zero-peer results; `min_seeders` counts unknown as 0. Not the attribute filters.
    fn base_passes(&self, f: &Filter, doc: DocId) -> bool {
        // Some event passes (its trust, or its publisher is trusted locally), as in Postgres.
        if f.min_trust > 0 && self.eligible_trust.get_val(doc) < f.min_trust {
            return false;
        }
        if f.categories != 0 && f.categories & (1 << self.category.get_val(doc).min(63)) == 0 {
            return false;
        }
        if f.min_size.is_some() || f.max_size.is_some() {
            let s = self.total_size.get_val(doc);
            if s == NONE || f.min_size.is_some_and(|m| s < m) || f.max_size.is_some_and(|m| s > m) {
                return false;
            }
        }
        if f.since.is_some() || f.until.is_some() {
            let c = self.created_at.get_val(doc);
            if f.since.is_some_and(|s| c < s) || f.until.is_some_and(|u| c > u) {
                return false;
            }
        }
        if f.hide_dead || f.min_seeders.is_some() {
            let best = self.best_peers.get_val(doc);
            if f.hide_dead && best == 0 && self.expires_at.get_val(doc) > f.now {
                return false;
            }
            if f.min_seeders.is_some_and(|m| best.max(0) < m) {
                return false;
            }
        }
        if let Some(m) = f.media_id
            && self.media_id.get_val(doc) != m
        {
            return false;
        }
        if let Some(r) = f.min_rating {
            let v = self.media_rating.get_val(doc);
            if v == NONE || v < r {
                return false;
            }
        }
        if f.min_year.is_some() || f.max_year.is_some() {
            let y = self.year.get_val(doc);
            if y == NONE || f.min_year.is_some_and(|m| y < m) || f.max_year.is_some_and(|m| y > m) {
                return false;
            }
        }
        true
    }

    /// Sort key: higher is better. Unknown sizes/peers sort last in both directions; ties go
    /// to the newest.
    fn key(&self, sort: SortBy, descending: bool, doc: DocId, score: Score) -> Key {
        let created = self.created_at.get_val(doc);
        let dir = |v: i64| if descending { v as f64 } else { -(v as f64) };
        let known = |v: i64| if v == NONE { f64::NEG_INFINITY } else { dir(v) };
        match sort {
            SortBy::Relevance => Key(f64::from(score), created),
            SortBy::Date => Key(dir(created), 0),
            SortBy::Size => Key(known(self.total_size.get_val(doc)), created),
            SortBy::Seeders => Key(known(self.best_peers.get_val(doc)), created),
            SortBy::Rating => Key(known(self.media_rating.get_val(doc)), created),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key(pub f64, pub i64);

/// A ranked hit; ordering = key, then the lower address first (deterministic pages).
#[derive(Clone, Copy, Debug)]
pub struct Ranked {
    pub key: Key,
    pub addr: DocAddress,
}

impl PartialEq for Ranked {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Ranked {
    fn cmp(&self, o: &Self) -> Ordering {
        self.key
            .0
            .total_cmp(&o.key.0)
            .then(self.key.1.cmp(&o.key.1))
            .then_with(|| {
                (o.addr.segment_ord, o.addr.doc_id).cmp(&(self.addr.segment_ord, self.addr.doc_id))
            })
    }
}

/// Bounded best-k: a min-heap of the k best seen so far.
struct TopK {
    k: usize,
    heap: BinaryHeap<Reverse<Ranked>>,
}

impl TopK {
    fn new(k: usize) -> Self {
        TopK {
            k,
            heap: BinaryHeap::with_capacity(k.min(4096) + 1),
        }
    }

    fn push(&mut self, r: Ranked) {
        if self.k == 0 {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(Reverse(r));
        } else if self.heap.peek().is_some_and(|Reverse(min)| r > *min) {
            self.heap.pop();
            self.heap.push(Reverse(r));
        }
    }

    fn into_sorted(self) -> Vec<Ranked> {
        let mut v: Vec<Ranked> = self.heap.into_iter().map(|Reverse(r)| r).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v
    }
}

/// Count + best `k` of the filtered hits.
pub struct TopFiltered {
    pub filter: Filter,
    pub sort: SortBy,
    pub descending: bool,
    pub k: usize,
}

pub struct TopSegment {
    cols: Columns,
    filter: Filter,
    sort: SortBy,
    descending: bool,
    segment: SegmentOrdinal,
    count: u64,
    top: TopK,
}

impl SegmentCollector for TopSegment {
    type Fruit = (u64, Vec<Ranked>);

    fn collect(&mut self, doc: DocId, score: Score) {
        if !self.cols.passes(&self.filter, doc) {
            return;
        }
        self.count += 1;
        let key = self.cols.key(self.sort, self.descending, doc, score);
        self.top.push(Ranked {
            key,
            addr: DocAddress::new(self.segment, doc),
        });
    }

    fn harvest(self) -> Self::Fruit {
        (self.count, self.top.into_sorted())
    }
}

impl Collector for TopFiltered {
    type Fruit = (u64, Vec<Ranked>);
    type Child = TopSegment;

    fn for_segment(
        &self,
        segment: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<TopSegment> {
        Ok(TopSegment {
            cols: Columns::load(reader, &self.filter)?,
            filter: self.filter.clone(),
            sort: self.sort,
            descending: self.descending,
            segment,
            count: 0,
            top: TopK::new(self.k),
        })
    }

    fn requires_scoring(&self) -> bool {
        self.sort == SortBy::Relevance
    }

    fn merge_fruits(&self, fruits: Vec<(u64, Vec<Ranked>)>) -> tantivy::Result<Self::Fruit> {
        let mut top = TopK::new(self.k);
        let mut count = 0;
        for (n, hits) in fruits {
            count += n;
            for h in hits {
                top.push(h);
            }
        }
        Ok((count, top.into_sorted()))
    }
}

/// One requested facet: its keyword fast field, and which `Filter::attrs` entry filters on
/// it (left out of its own counts).
#[derive(Clone, Debug)]
pub struct FacetField {
    pub field: String,
    pub filter: Option<usize>,
}

/// Value counts of several keyword fast fields over the filtered hits, disjunctively.
pub struct FacetCounts {
    pub filter: Filter,
    pub fields: Vec<FacetField>,
}

/// Per facet: documents counted, of which without any value, and value → count.
#[derive(Clone, Debug, Default)]
pub struct FacetTally {
    pub counted: u64,
    pub missing: u64,
    pub values: HashMap<String, u64>,
}

/// Documents passing every filter, and one tally per facet.
pub type FacetFruit = (u64, Vec<FacetTally>);

pub struct FacetSegment {
    cols: Columns,
    filter: Filter,
    fields: Vec<FacetField>,
    columns: Vec<Option<StrColumn>>,
    counts: Vec<Vec<u64>>,
    totals: Vec<u64>,
    missing: Vec<u64>,
    all: u64,
}

impl FacetSegment {
    fn count(&mut self, i: usize, doc: DocId) {
        self.totals[i] += 1;
        let mut any = false;
        if let Some(c) = &self.columns[i] {
            for ord in c.ords().values_for_doc(doc) {
                any = true;
                if let Some(n) = self.counts[i].get_mut(ord as usize) {
                    *n += 1;
                }
            }
        }
        if !any {
            self.missing[i] += 1;
        }
    }
}

impl SegmentCollector for FacetSegment {
    type Fruit = FacetFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if !self.cols.base_passes(&self.filter, doc) {
            return;
        }
        match self.cols.attr_failures(doc) {
            (0, _) => {
                self.all += 1;
                for i in 0..self.fields.len() {
                    self.count(i, doc);
                }
            }
            (1, k) => {
                for i in 0..self.fields.len() {
                    if self.fields[i].filter == Some(k) {
                        self.count(i, doc);
                    }
                }
            }
            _ => {}
        }
    }

    fn harvest(self) -> Self::Fruit {
        let mut out = Vec::with_capacity(self.fields.len());
        for (i, col) in self.columns.iter().enumerate() {
            let mut m = HashMap::new();
            if let Some(c) = col {
                let mut s = String::new();
                for (ord, n) in self.counts[i].iter().enumerate().filter(|(_, n)| **n > 0) {
                    s.clear();
                    if c.ord_to_str(ord as u64, &mut s).unwrap_or(false) {
                        m.insert(s.clone(), *n);
                    }
                }
            }
            out.push(FacetTally {
                counted: self.totals[i],
                missing: self.missing[i],
                values: m,
            });
        }
        (self.all, out)
    }
}

impl Collector for FacetCounts {
    type Fruit = FacetFruit;
    type Child = FacetSegment;

    fn for_segment(
        &self,
        _: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<FacetSegment> {
        let mut columns = Vec::with_capacity(self.fields.len());
        let mut counts = Vec::with_capacity(self.fields.len());
        for f in &self.fields {
            let column = reader.fast_fields().str(&f.field)?;
            counts.push(vec![0; column.as_ref().map_or(0, |c| c.num_terms())]);
            columns.push(column);
        }
        Ok(FacetSegment {
            cols: Columns::load(reader, &self.filter)?,
            filter: self.filter.clone(),
            fields: self.fields.clone(),
            columns,
            counts,
            totals: vec![0; self.fields.len()],
            missing: vec![0; self.fields.len()],
            all: 0,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Self::Fruit>) -> tantivy::Result<Self::Fruit> {
        let mut all = 0;
        let mut out = vec![FacetTally::default(); self.fields.len()];
        for (n_all, fruit) in fruits {
            all += n_all;
            for (i, t) in fruit.into_iter().enumerate() {
                out[i].counted += t.counted;
                out[i].missing += t.missing;
                for (k, v) in t.values {
                    *out[i].values.entry(k).or_default() += v;
                }
            }
        }
        Ok((all, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(k: f64, c: i64, doc: u32) -> Ranked {
        Ranked {
            key: Key(k, c),
            addr: DocAddress::new(0, doc),
        }
    }

    #[test]
    fn top_k_keeps_the_best_in_order() {
        let mut t = TopK::new(3);
        for (i, k) in [5.0, 1.0, 9.0, 7.0, 3.0, 9.0].into_iter().enumerate() {
            t.push(r(k, 0, i as u32));
        }
        let got: Vec<(f64, u32)> = t
            .into_sorted()
            .iter()
            .map(|x| (x.key.0, x.addr.doc_id))
            .collect();
        // Equal keys: the lower doc id first.
        assert_eq!(got, vec![(9.0, 2), (9.0, 5), (7.0, 3)]);
        let mut none = TopK::new(0);
        none.push(r(1.0, 0, 0));
        assert!(none.into_sorted().is_empty());
    }

    #[test]
    fn unknown_sorts_last_both_ways() {
        let unknown = Key(f64::NEG_INFINITY, 10);
        assert!(
            r(-1e18, 0, 0)
                > Ranked {
                    key: unknown,
                    addr: DocAddress::new(0, 1)
                }
        );
        assert!(r(0.0, 5, 0) > r(0.0, 4, 1), "ties: newer first");
    }
}
