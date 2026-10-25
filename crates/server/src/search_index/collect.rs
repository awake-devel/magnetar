//! One-pass collectors over the matching documents: every filter of `SearchQuery` is applied
//! on fast columns, then the hit is counted and kept if it is in the top `offset + limit` for
//! the sort. Counting comes for free, so totals are exact on the index path.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::{ColumnValues, StrColumn};
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal, SegmentReader};

use super::schema::NONE;

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
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Relevance,
    Date,
    Size,
    Seeders,
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
}

impl Columns {
    fn load(seg: &SegmentReader) -> tantivy::Result<Self> {
        let ff = seg.fast_fields();
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
        })
    }

    /// ARCHITECTURE.md §5 semantics: unknown size fails a size bound; `hide_dead` drops only
    /// fresh zero-peer results; `min_seeders` counts unknown as 0.
    fn passes(&self, f: &Filter, doc: DocId) -> bool {
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
            cols: Columns::load(reader)?,
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

/// Value counts of one keyword fast field over the filtered hits.
pub struct FacetCounts {
    pub filter: Filter,
    pub field: String,
}

pub struct FacetSegment {
    cols: Columns,
    filter: Filter,
    column: Option<StrColumn>,
    counts: Vec<u64>,
    total: u64,
}

impl SegmentCollector for FacetSegment {
    type Fruit = (u64, HashMap<String, u64>);

    fn collect(&mut self, doc: DocId, _score: Score) {
        if !self.cols.passes(&self.filter, doc) {
            return;
        }
        self.total += 1;
        if let Some(c) = &self.column {
            for ord in c.ords().values_for_doc(doc) {
                if let Some(n) = self.counts.get_mut(ord as usize) {
                    *n += 1;
                }
            }
        }
    }

    fn harvest(self) -> Self::Fruit {
        let mut out = HashMap::new();
        if let Some(c) = &self.column {
            let mut s = String::new();
            for (ord, n) in self.counts.iter().enumerate().filter(|(_, n)| **n > 0) {
                s.clear();
                if c.ord_to_str(ord as u64, &mut s).unwrap_or(false) {
                    out.insert(s.clone(), *n);
                }
            }
        }
        (self.total, out)
    }
}

impl Collector for FacetCounts {
    type Fruit = (u64, HashMap<String, u64>);
    type Child = FacetSegment;

    fn for_segment(
        &self,
        _: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<FacetSegment> {
        let column = reader.fast_fields().str(&self.field)?;
        let terms = column.as_ref().map_or(0, |c| c.num_terms());
        Ok(FacetSegment {
            cols: Columns::load(reader)?,
            filter: self.filter.clone(),
            column,
            counts: vec![0; terms],
            total: 0,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Self::Fruit>) -> tantivy::Result<Self::Fruit> {
        let mut total = 0;
        let mut out: HashMap<String, u64> = HashMap::new();
        for (n, m) in fruits {
            total += n;
            for (k, v) in m {
                *out.entry(k).or_default() += v;
            }
        }
        Ok((total, out))
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
