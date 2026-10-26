//! Embedded Tantivy search index (ARCHITECTURE.md D43).
//!
//! Postgres stays the source of truth; this index is derived from it and lives in
//! `MAGNETAR_SEARCH_INDEX_DIR` (default `<state dir>/search-index`). One document per infohash
//! (canonical event as in D6, blocked publishers' events left out). The index is rebuilt when
//! it is missing, unreadable, from another database, or its `SCHEMA_VERSION` changed; a full
//! build commits every `BUILD_COMMIT_EVERY` infohashes with its keyset cursor, so a restart
//! resumes it. Until the first build completes, searches use the Postgres query.
//!
//! Updates: triggers append every relevant change (torrents, liveness, torrent_media, media,
//! local_overrides) to `search_changes`; `index_loop` re-indexes the changed infohashes about
//! every second in one commit, then deletes the log rows it read.

pub mod collect;
pub mod load;
pub mod query;
pub mod schema;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use magnetar_core::api::{FacetCount, FacetSet, FacetsResponse, SearchIndexStatus};
use magnetar_core::enrich::ids::split_media_token;
use magnetar_core::error::{Error, Result};
use magnetar_core::model::{MediaKind, TorrentSummary, npub};
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tantivy::DocSet;
use tantivy::SegmentMeta;
use tantivy::index::SegmentId;
use tantivy::merge_policy::{LogMergePolicy, MergeCandidate, MergePolicy};
use tantivy::query::{EnableScoring, Query};
use tantivy::schema::{Field, Value};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};
use tokio::sync::watch;

use self::collect::{AttrFilter, FacetCounts, FacetField, FacetTally, Filter, SortBy, TopFiltered};
use self::schema::{Fields, IndexDoc, NONE, SCHEMA_VERSION, TOKENIZER, analyzer};
use crate::daemons::stop_requested;

/// Changes applied per commit (one `search_changes` read).
pub const APPLY_BATCH: i64 = 5000;
/// Pause between change-log polls when the log was drained.
pub const APPLY_EVERY: Duration = Duration::from_secs(1);
/// Infohashes per document query during a full build, and queries in flight.
pub const BUILD_CHUNK: usize = 2000;
pub const BUILD_PARALLEL: usize = 4;
/// A full build commits (and records its cursor) after this many infohashes.
pub const BUILD_COMMIT_EVERY: u64 = 200_000;
/// Indexing memory over all writer threads.
pub const WRITER_MEMORY: usize = 128 << 20;
/// Facet values returned at most.
pub const MAX_FACET_VALUES: usize = 200;

pub(crate) fn ix(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("search index: {e}"))
}

/// Segments aim at this share of the index (at least `MIN_SEGMENT_DOCS`), so a query over
/// every document fans out over ~16 segments on the search threads instead of a few huge ones:
/// at 3.2 M documents Tantivy's default policy left segments of 1.04 M, 995 k and 684 k docs,
/// and the biggest one bounded the latency of every broad query.
pub const SEGMENTS_TARGET: u32 = 16;
pub const MIN_SEGMENT_DOCS: u32 = 200_000;

/// `LogMergePolicy`, with merges split so that no merged segment exceeds the cap.
#[derive(Debug, Default)]
struct CappedMergePolicy {
    inner: LogMergePolicy,
}

impl MergePolicy for CappedMergePolicy {
    fn compute_merge_candidates(&self, segments: &[SegmentMeta]) -> Vec<MergeCandidate> {
        let total: u32 = segments.iter().map(|s| s.num_docs()).sum();
        let cap = (total / SEGMENTS_TARGET).max(MIN_SEGMENT_DOCS);
        let docs: HashMap<SegmentId, u32> =
            segments.iter().map(|s| (s.id(), s.num_docs())).collect();
        let mut inner = self.inner.clone();
        inner.set_max_docs_before_merge(cap as usize);
        let mut out = Vec::new();
        for MergeCandidate(ids) in inner.compute_merge_candidates(segments) {
            if ids.len() == 1 {
                // Rewriting one segment to drop its deleted documents.
                out.push(MergeCandidate(ids));
                continue;
            }
            let mut group: Vec<SegmentId> = Vec::new();
            let mut sum = 0u32;
            for id in ids {
                let n = docs.get(&id).copied().unwrap_or(0);
                if !group.is_empty() && sum + n > cap {
                    if group.len() > 1 {
                        out.push(MergeCandidate(std::mem::take(&mut group)));
                    }
                    group.clear();
                    sum = 0;
                }
                group.push(id);
                sum += n;
            }
            if group.len() > 1 {
                out.push(MergeCandidate(group));
            }
        }
        out
    }
}

/// Back to the last commit. Tantivy replaces the writer, merge policy included.
fn rollback(w: &mut IndexWriter<TantivyDocument>) -> Result<()> {
    w.rollback().map_err(ix)?;
    w.set_merge_policy(Box::new(CappedMergePolicy::default()));
    Ok(())
}

/// Commit payload: what the index is, and how far a full build got.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Payload {
    schema: u32,
    db_id: String,
    complete: bool,
    /// Last infohash of the last committed build chunk (build incomplete).
    cursor: Option<String>,
    /// Highest `search_changes` id whose change the index holds (at a fresh build: the
    /// `meta.search_applied` of its start). Compared with the database on open (`open_for`).
    applied: i64,
}

#[derive(Default)]
struct Progress {
    building: bool,
    done: u64,
    total: u64,
    build_secs: Option<u64>,
    last_commit: Option<i64>,
    error: Option<String>,
}

pub struct SearchIndex {
    dir: PathBuf,
    fields: Fields,
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    payload: Mutex<Payload>,
    ready: AtomicBool,
    progress: Mutex<Progress>,
}

/// One index query: analysed words plus the resolved filters.
#[derive(Clone, Debug)]
pub struct IndexQuery {
    pub words: Vec<String>,
    pub filter: Filter,
    pub sort: SortBy,
    pub descending: bool,
    pub limit: usize,
    pub offset: usize,
}

pub struct IndexHit {
    pub summary: TorrentSummary,
    /// Not every word is in the title, and some word matched a file name: worth looking for
    /// the matching file (`matched_file`).
    pub file_matched: bool,
}

pub struct IndexResult {
    pub hits: Vec<IndexHit>,
    pub total: u64,
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Files Tantivy keeps in an index directory: only a directory holding one of them (or an
/// empty one) is ever wiped, so a mistyped `MAGNETAR_SEARCH_INDEX_DIR` cannot delete user data.
const INDEX_MARKERS: &[&str] = &["meta.json", ".managed.json"];

pub fn is_index_dir(dir: &Path) -> bool {
    INDEX_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Make `dir` an empty directory: created when missing, emptied when it holds an index.
fn clear_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let empty = std::fs::read_dir(dir).map_err(ix)?.next().is_none();
        if !empty {
            if !is_index_dir(dir) {
                return Err(ix(format!(
                    "{} is not empty and holds no search index: not deleting it",
                    dir.display()
                )));
            }
            std::fs::remove_dir_all(dir).map_err(ix)?;
        }
    }
    std::fs::create_dir_all(dir).map_err(ix)
}

impl SearchIndex {
    /// Open the index in `dir`, or start an empty one (to be built) when there is none, it
    /// cannot be read, or it belongs to another schema version or database. Blocking.
    pub fn open(dir: &Path, db_id: &str) -> Result<Self> {
        let fields = Fields::new();
        match Self::open_existing(dir, &fields, db_id) {
            Ok(Some((index, payload))) => {
                match Self::with_index(dir, fields.clone(), index, payload) {
                    Ok(idx) => return Ok(idx),
                    // Another process holds the writer lock: its index, not ours to wipe.
                    Err(e @ tantivy::TantivyError::LockFailure(..)) => return Err(ix(e)),
                    Err(e) => {
                        tracing::warn!("search index in {}: {e}; rebuilding", dir.display())
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("search index in {}: {e}; rebuilding", dir.display()),
        }
        clear_dir(dir)?;
        let index = Index::create_in_dir(dir, fields.schema.clone()).map_err(ix)?;
        let p = Payload {
            schema: SCHEMA_VERSION,
            db_id: db_id.to_string(),
            ..Payload::default()
        };
        Self::with_index(dir, fields, index, p).map_err(ix)
    }

    fn open_existing(dir: &Path, fields: &Fields, db_id: &str) -> Result<Option<(Index, Payload)>> {
        if !dir.join("meta.json").exists() {
            return Ok(None);
        }
        let index = Index::open_in_dir(dir).map_err(ix)?;
        let p: Payload = index
            .load_metas()
            .map_err(ix)?
            .payload
            .and_then(|p| serde_json::from_str(&p).ok())
            .unwrap_or_default();
        if p.schema != SCHEMA_VERSION || p.db_id != db_id || index.schema() != fields.schema {
            tracing::info!(
                "search index in {}: schema {} (want {SCHEMA_VERSION}) or database changed; rebuilding",
                dir.display(),
                p.schema
            );
            return Ok(None);
        }
        Ok(Some((index, p)))
    }

    /// Writer first (it takes the directory lock), then the reader, which opens every segment
    /// and so fails on missing or damaged files.
    fn with_index(
        dir: &Path,
        fields: Fields,
        mut index: Index,
        payload: Payload,
    ) -> tantivy::Result<Self> {
        index.tokenizers().register(TOKENIZER, analyzer());
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        // Queries fan out over segments; leave a core or two for Postgres and the API.
        index.set_multithread_executor(cpus.saturating_sub(2).clamp(1, 16))?;
        let writer: IndexWriter<TantivyDocument> =
            index.writer_with_num_threads(cpus.clamp(1, 4), WRITER_MEMORY)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        writer.set_merge_policy(Box::new(CappedMergePolicy::default()));
        let ready = payload.complete;
        Ok(SearchIndex {
            dir: dir.to_path_buf(),
            fields,
            reader,
            writer: Mutex::new(writer),
            payload: Mutex::new(payload),
            ready: AtomicBool::new(ready),
            progress: Mutex::default(),
        })
    }

    /// Async wrapper of `open` (it reads the directory and may wipe it).
    pub async fn open_async(dir: PathBuf, db_id: String) -> Result<Arc<Self>> {
        tokio::task::spawn_blocking(move || Self::open(&dir, &db_id).map(Arc::new))
            .await
            .map_err(ix)?
    }

    /// `open_async` for the database behind `pool`, checked against its change log
    /// (`rebuild_if_out_of_step`). What `serve` uses.
    pub async fn open_for(pool: &PgPool, dir: PathBuf) -> Result<Arc<Self>> {
        let id = load::database_id(pool).await?;
        let idx = Self::open_async(dir, id).await?;
        let (acked, last_id) = load::log_position(pool).await?;
        idx.rebuild_if_out_of_step(acked, last_id);
        Ok(idx)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn commit(&self, w: &mut IndexWriter<TantivyDocument>, p: &Payload) -> Result<()> {
        let mut pc = w.prepare_commit().map_err(ix)?;
        pc.set_payload(&serde_json::to_string(p).map_err(ix)?);
        pc.commit().map_err(ix)?;
        self.reader.reload().map_err(ix)?;
        self.progress.lock().unwrap().last_commit = Some(magnetar_core::now());
        Ok(())
    }

    /// Start or resume a full build; returns the keyset cursor to continue after. Blocking.
    pub fn begin_build(&self) -> Result<String> {
        let p = self.payload.lock().unwrap().clone();
        let mut w = self.writer.lock().unwrap();
        // A failed attempt in this process may have left documents after its last commit.
        rollback(&mut w)?;
        let cursor = match &p.cursor {
            Some(c) if !p.complete => c.clone(),
            _ => {
                w.delete_all_documents().map_err(ix)?;
                let fresh = Payload {
                    complete: false,
                    cursor: None,
                    ..p
                };
                self.commit(&mut w, &fresh)?;
                *self.payload.lock().unwrap() = fresh;
                String::new()
            }
        };
        self.ready.store(false, Ordering::Release);
        let mut pr = self.progress.lock().unwrap();
        pr.building = true;
        pr.error = None;
        pr.done = self.num_docs();
        Ok(cursor)
    }

    /// Add one build chunk whose last infohash is `cursor`; commit when asked. Blocking.
    pub fn add_build_chunk(&self, docs: Vec<IndexDoc>, cursor: &str, commit: bool) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        for d in &docs {
            w.add_document(self.fields.document(d)).map_err(ix)?;
        }
        if commit {
            let mut p = self.payload.lock().unwrap().clone();
            p.cursor = Some(cursor.to_string());
            self.commit(&mut w, &p)?;
            *self.payload.lock().unwrap() = p;
        }
        Ok(())
    }

    /// Mark the build complete: searches switch to the index. Blocking.
    pub fn finish_build(&self, secs: u64) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        let mut p = self.payload.lock().unwrap().clone();
        p.complete = true;
        p.cursor = None;
        self.commit(&mut w, &p)?;
        *self.payload.lock().unwrap() = p;
        self.ready.store(true, Ordering::Release);
        let mut pr = self.progress.lock().unwrap();
        pr.building = false;
        pr.build_secs = Some(secs);
        Ok(())
    }

    /// Replace the documents of `infohashes` with `docs` (absent = deleted) in one commit that
    /// records `applied`, the highest change log id they cover. Blocking.
    pub fn replace(&self, infohashes: &[String], docs: Vec<IndexDoc>, applied: i64) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        for ih in infohashes {
            w.delete_term(self.fields.infohash_term(ih));
        }
        let mut res = docs
            .iter()
            .try_for_each(|d| w.add_document(self.fields.document(d)).map(drop))
            .map_err(ix);
        if res.is_ok() {
            let mut p = self.payload.lock().unwrap().clone();
            p.applied = p.applied.max(applied);
            res = self.commit(&mut w, &p);
            if res.is_ok() {
                *self.payload.lock().unwrap() = p;
            }
        }
        if res.is_err() {
            // Half a batch must not ride along with the next commit; the batch is retried.
            let _ = rollback(&mut w);
        }
        res
    }

    /// A complete index out of step with the change log of its database is rebuilt: the log
    /// was acknowledged beyond what it holds (an index directory restored from an older backup,
    /// or a second index following the same log), or the log is behind it (the database was
    /// restored from an older dump). `acked` = `meta.search_applied`, `last_id` = the last id
    /// handed out. Returns whether a rebuild was scheduled.
    pub fn rebuild_if_out_of_step(&self, acked: i64, last_id: i64) -> bool {
        let mut p = self.payload.lock().unwrap();
        if !p.complete || (acked <= p.applied && p.applied <= last_id) {
            return false;
        }
        tracing::warn!(
            "search index in {}: holds changes up to {}, the database's change log is at {acked} \
             acknowledged / {last_id} issued; rebuilding",
            self.dir.display(),
            p.applied
        );
        p.complete = false;
        p.cursor = None;
        self.ready.store(false, Ordering::Release);
        true
    }

    /// A fresh build reflects the database from now on: the changes acknowledged so far are in.
    fn start_applied_at(&self, acked: i64) {
        self.payload.lock().unwrap().applied = acked;
    }

    fn set_build_progress(&self, done: u64, total: u64) {
        let mut pr = self.progress.lock().unwrap();
        pr.done = done;
        pr.total = total;
    }

    fn set_error(&self, e: &Error) {
        self.progress.lock().unwrap().error = Some(e.to_string());
    }

    fn query(&self, searcher: &tantivy::Searcher, words: &[String]) -> Box<dyn Query> {
        let max_df = query::fuzzy_max_df(searcher.num_docs());
        query::build(
            &self.fields,
            words,
            &|t| searcher.doc_freq(t).unwrap_or(0),
            max_df,
        )
    }

    /// Run one query. Blocking (CPU-bound: call through `spawn_blocking`).
    pub fn search(&self, q: &IndexQuery) -> Result<IndexResult> {
        let searcher = self.reader.searcher();
        let query = self.query(&searcher, &q.words);
        let collector = TopFiltered {
            filter: q.filter.clone(),
            sort: q.sort,
            descending: q.descending,
            k: q.offset + q.limit,
        };
        let (total, top) = searcher.search(&*query, &collector).map_err(ix)?;
        // Which hits matched a word in their file names (most text hits match in the title, and
        // many of the others only in the description): one seek per hit.
        let files = (!q.words.is_empty())
            .then(|| {
                query::files_any(&self.fields, &q.words)
                    .weight(EnableScoring::disabled_from_searcher(&searcher))
            })
            .transpose()
            .map_err(ix)?;
        let mut hits = Vec::with_capacity(q.limit);
        for r in top.into_iter().skip(q.offset).take(q.limit) {
            let doc: TantivyDocument = searcher.doc(r.addr).map_err(ix)?;
            let rank = if q.sort == SortBy::Relevance {
                r.key.0 as f32
            } else {
                0.0
            };
            let (summary, titles) = self.summary(&doc, rank, &q.filter);
            let file_matched = match &files {
                Some(w) if !query::all_match(&q.words, &titles) => {
                    let seg = searcher.segment_reader(r.addr.segment_ord);
                    let mut sc = w.scorer(seg, 1.0).map_err(ix)?;
                    sc.seek(r.addr.doc_id) == r.addr.doc_id
                }
                _ => false,
            };
            hits.push(IndexHit {
                summary,
                file_matched,
            });
        }
        Ok(IndexResult { hits, total })
    }

    /// Full tallies of facets `names` over the hits of `q` (limit/offset/sort ignored), each
    /// without its own filter (D44): (hits with every filter, one tally per name). Ok(None)
    /// when a name is unknown. Blocking.
    pub fn facet_tallies(
        &self,
        q: &IndexQuery,
        names: &[String],
    ) -> Result<Option<(u64, Vec<FacetTally>)>> {
        let mut fields = Vec::with_capacity(names.len());
        for name in names {
            let Some(field) = self.fields.facet(name) else {
                return Ok(None);
            };
            let field = self.fields.schema.get_field_name(field).to_string();
            let filter = q.filter.attrs.iter().position(|a| a.field == field);
            fields.push(FacetField { field, filter });
        }
        let searcher = self.reader.searcher();
        let query = self.query(&searcher, &q.words);
        let collector = FacetCounts {
            filter: q.filter.clone(),
            fields,
        };
        Ok(Some(searcher.search(&*query, &collector).map_err(ix)?))
    }

    /// Value counts of facets `names` (`facet_tallies`, most frequent `size` values). Blocking.
    pub fn facet_set(
        &self,
        q: &IndexQuery,
        names: &[String],
        size: usize,
    ) -> Result<Option<FacetSet>> {
        let Some((total, tallies)) = self.facet_tallies(q, names)? else {
            return Ok(None);
        };
        let facets = names
            .iter()
            .zip(tallies)
            .map(|(name, t)| {
                let mut counts = sorted_counts(t.values);
                counts.truncate(size.clamp(1, MAX_FACET_VALUES));
                FacetsResponse {
                    field: name.clone(),
                    total: t.counted,
                    counts,
                }
            })
            .collect();
        Ok(Some(FacetSet { total, facets }))
    }

    /// Value counts of one facet (`facet_set` with one name). Blocking.
    pub fn facets(
        &self,
        q: &IndexQuery,
        name: &str,
        size: usize,
    ) -> Result<Option<FacetsResponse>> {
        Ok(self
            .facet_set(q, &[name.to_string()], size)?
            .and_then(|mut s| s.facets.pop()))
    }

    /// Stored fields → summary, plus all titles joined (for the title-match check).
    fn summary(
        &self,
        doc: &TantivyDocument,
        rank: f32,
        filter: &Filter,
    ) -> (TorrentSummary, String) {
        let now = filter.now;
        let f = &self.fields;
        let text = |field: Field| -> Option<String> {
            doc.get_first(field)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let int = |field: Field| -> i64 {
            doc.get_first(field)
                .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                .unwrap_or(NONE)
        };
        let opt_u32 = |field: Field| u32::try_from(int(field)).ok();
        let titles: Vec<&str> = doc.get_all(f.title).filter_map(|v| v.as_str()).collect();
        let pubkey = text(f.pubkey).unwrap_or_default();
        let best = int(f.best_peers);
        let expires = int(f.expires_at);
        let dead = (best != NONE && now < expires).then_some(best == 0);
        let summary = TorrentSummary {
            infohash: text(f.infohash).unwrap_or_default(),
            event_id: text(f.event_id).unwrap_or_default(),
            title: titles.first().map(|t| t.to_string()).unwrap_or_default(),
            category: schema::category_from_code(u64::try_from(int(f.category)).unwrap_or(0)),
            total_size: u64::try_from(int(f.total_size)).ok(),
            file_count: opt_u32(f.file_count).unwrap_or(0),
            created_at: int(f.created_at),
            npub: npub(&pubkey),
            pubkey,
            seeders: opt_u32(f.seeders),
            leechers: opt_u32(f.leechers),
            dead,
            trust: int(f.trust).clamp(0, 100) as u8,
            pow_bits: int(f.pow_bits).clamp(0, 255) as u8,
            // Publishers with an event passing the threshold, as the Postgres search counts.
            publisher_count: doc
                .get_all(f.publisher_trust)
                .filter_map(|v| v.as_i64())
                .filter(|t| *t >= filter.min_trust)
                .count()
                .max(1) as u32,
            source_url: text(f.source_url),
            rank,
            pinned: false,
            hidden: false,
            media_title: text(f.media_title),
            media_year: i32::try_from(int(f.media_year)).ok().filter(|y| *y >= 0),
            media_kind: text(f.media_kind).and_then(|k| k.parse::<MediaKind>().ok()),
            matched_file: None,
            media_id: Some(int(f.media_id)).filter(|id| *id != NONE),
            media_rating: Some(int(f.media_rating))
                .filter(|r| *r != NONE)
                .map(|r| r as f32 / 10.0),
            attrs: text(f.attrs).and_then(|j| serde_json::from_str(&j).ok()),
        };
        (summary, titles.join(" "))
    }

    /// Index-side status (the change-log backlog is added by the caller).
    pub fn status(&self) -> SearchIndexStatus {
        let pr = self.progress.lock().unwrap();
        let state = if pr.error.is_some() && !self.is_ready() {
            "error"
        } else if self.is_ready() {
            "ready"
        } else {
            "building"
        };
        SearchIndexStatus {
            state: state.into(),
            docs: self.num_docs(),
            build_done: pr.done,
            build_total: pr.total,
            build_secs: pr.build_secs,
            last_commit: pr.last_commit,
            pending: 0,
            lag_secs: None,
            bytes: dir_bytes(&self.dir),
            error: pr.error.clone(),
        }
    }
}

/// Most frequent first, ties by value.
pub fn sorted_counts(values: HashMap<String, u64>) -> Vec<FacetCount> {
    let mut counts: Vec<FacetCount> = values
        .into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect();
    counts.sort_unstable_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    counts
}

fn join_err(e: tokio::task::JoinError) -> Error {
    ix(e)
}

/// `SearchQuery` → `IndexQuery` (`text` already stripped of a media token).
pub fn index_query(
    q: &SearchQuery,
    text: &str,
    min_trust: u8,
    media_id: Option<i64>,
    now: i64,
) -> IndexQuery {
    let sort = match q.effective_sort() {
        Sort::Relevance if !query::tokens(text).is_empty() => SortBy::Relevance,
        // A bare media token has no words to rank by.
        Sort::Relevance | Sort::Date => SortBy::Date,
        Sort::Size => SortBy::Size,
        Sort::Seeders => SortBy::Seeders,
        Sort::Rating => SortBy::Rating,
    };
    let to_i64 = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    IndexQuery {
        words: query::tokens(text),
        filter: Filter {
            categories: q
                .categories
                .iter()
                .fold(0, |m, c| m | 1 << schema::category_code(*c)),
            min_size: q.min_size.map(to_i64),
            max_size: q.max_size.map(to_i64),
            since: q.since,
            until: q.until,
            min_trust: i64::from(min_trust),
            hide_dead: q.hide_dead,
            min_seeders: q.min_seeders.map(i64::from),
            media_id,
            min_rating: q.min_rating.map(|r| (r * 10.0).round() as i64),
            min_year: q.min_year.map(i64::from),
            max_year: q.max_year.map(i64::from),
            attrs: q
                .attr_filters()
                .into_iter()
                .map(|(name, values)| AttrFilter {
                    field: schema::facet_field_name(name),
                    values,
                })
                .collect(),
            hide_adult: q.hides_adult(),
            now,
        },
        sort,
        // Relevance is always best-first (as the Postgres search).
        descending: sort == SortBy::Relevance || q.descending,
        limit: q.effective_limit() as usize,
        offset: q.offset as usize,
    }
}

/// Resolve a media token; Ok(None) = it names nothing, so nothing can match.
async fn media_filter(pool: &PgPool, text: &str) -> Result<Option<(String, Option<i64>)>> {
    let (rest, media) = split_media_token(text);
    match media {
        None => Ok(Some((rest, None))),
        Some(r) => Ok(load::resolve_media(pool, &r)
            .await?
            .map(|id| (rest, Some(id)))),
    }
}

/// `PgStore::search` through the index (D43).
pub async fn search(
    pool: &PgPool,
    idx: &Arc<SearchIndex>,
    q: &SearchQuery,
    min_trust: u8,
    now: i64,
) -> Result<SearchPage> {
    let empty = SearchPage {
        hits: Vec::new(),
        total: 0,
        source: "server".into(),
        total_is_lower_bound: false,
    };
    let Some((text, media_id)) = media_filter(pool, &q.text).await? else {
        return Ok(empty);
    };
    let iq = index_query(q, &text, min_trust, media_id, now);
    let words = iq.words.clone();
    let res = {
        let idx = idx.clone();
        tokio::task::spawn_blocking(move || idx.search(&iq))
            .await
            .map_err(join_err)??
    };
    let need: Vec<String> = res
        .hits
        .iter()
        .filter(|h| h.file_matched)
        .map(|h| h.summary.event_id.clone())
        .collect();
    let files = load::event_files(pool, &need).await?;
    let total = res.total;
    // Tokenising up to MATCHED_FILE_PATHS paths per hit is CPU work: off the runtime.
    let hits = tokio::task::spawn_blocking(move || {
        res.hits
            .into_iter()
            .map(|h| {
                let mut s = h.summary;
                if h.file_matched {
                    s.matched_file = files
                        .get(&s.event_id)
                        .and_then(|f| query::best_file(&words, f.iter().map(String::as_str)));
                }
                s
            })
            .collect()
    })
    .await
    .map_err(join_err)?;
    Ok(SearchPage {
        hits,
        total,
        ..empty
    })
}

/// Facet counts through the index (D44: several at once, disjunctive); Ok(None) for an
/// unknown facet name.
pub async fn facet_set(
    pool: &PgPool,
    idx: &Arc<SearchIndex>,
    q: &SearchQuery,
    names: &[String],
    size: usize,
    min_trust: u8,
    now: i64,
) -> Result<Option<FacetSet>> {
    let Some((text, media_id)) = media_filter(pool, &q.text).await? else {
        // A media token naming nothing: every facet is empty.
        let facets = names
            .iter()
            .map(|n| FacetsResponse {
                field: n.clone(),
                ..Default::default()
            })
            .collect();
        return Ok(Some(FacetSet { total: 0, facets }));
    };
    let iq = index_query(q, &text, min_trust, media_id, now);
    let idx = idx.clone();
    let names = names.to_vec();
    tokio::task::spawn_blocking(move || idx.facet_set(&iq, &names, size))
        .await
        .map_err(join_err)?
}

/// Full facet tallies through the index (D47 breakdowns: top values, the rest, results without
/// a value); Ok(None) for an unknown facet name.
pub async fn facet_tallies(
    pool: &PgPool,
    idx: &Arc<SearchIndex>,
    q: &SearchQuery,
    names: &[String],
    min_trust: u8,
    now: i64,
) -> Result<Option<(u64, Vec<FacetTally>)>> {
    let Some((text, media_id)) = media_filter(pool, &q.text).await? else {
        return Ok(Some((0, vec![FacetTally::default(); names.len()])));
    };
    let iq = index_query(q, &text, min_trust, media_id, now);
    let idx = idx.clone();
    let names = names.to_vec();
    tokio::task::spawn_blocking(move || idx.facet_tallies(&iq, &names))
        .await
        .map_err(join_err)?
}

/// Full build (or resume): walks `torrent_infohashes`, documents loaded `BUILD_PARALLEL`
/// chunks at a time; indexing happens on blocking threads, never on the runtime.
pub async fn build(pool: &PgPool, idx: &Arc<SearchIndex>) -> Result<u64> {
    let started = Instant::now();
    let total = load::infohash_count(pool).await?;
    let (acked, _) = load::log_position(pool).await?;
    let mut cursor = {
        let idx = idx.clone();
        tokio::task::spawn_blocking(move || idx.begin_build())
            .await
            .map_err(join_err)??
    };
    if cursor.is_empty() {
        idx.start_applied_at(acked);
    }
    let mut done = idx.num_docs();
    idx.set_build_progress(done, total);
    tracing::info!(
        "search index: building {total} infohashes into {}{}",
        idx.dir().display(),
        if cursor.is_empty() {
            String::new()
        } else {
            format!(" (resuming after {cursor})")
        }
    );
    let mut since_commit = 0u64;
    loop {
        let page = load::infohashes_after(pool, &cursor, (BUILD_CHUNK * BUILD_PARALLEL * 2) as i64)
            .await?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        let chunks: Vec<Vec<String>> = page.chunks(BUILD_CHUNK).map(<[String]>::to_vec).collect();
        let mut loaded = futures::stream::iter(chunks)
            .map(|c| async move {
                let docs = load::load_docs(pool, &c).await?;
                Ok::<_, Error>((c, docs))
            })
            .buffered(BUILD_PARALLEL);
        while let Some(res) = loaded.next().await {
            let (chunk, docs) = res?;
            let chunk_last = chunk.last().cloned().unwrap_or_default();
            since_commit += chunk.len() as u64;
            let commit = since_commit >= BUILD_COMMIT_EVERY;
            if commit {
                since_commit = 0;
            }
            let idx2 = idx.clone();
            tokio::task::spawn_blocking(move || idx2.add_build_chunk(docs, &chunk_last, commit))
                .await
                .map_err(join_err)??;
            done += chunk.len() as u64;
            idx.set_build_progress(done, total.max(done));
        }
        cursor = last;
    }
    let secs = started.elapsed().as_secs();
    {
        let idx = idx.clone();
        tokio::task::spawn_blocking(move || idx.finish_build(secs))
            .await
            .map_err(join_err)??;
    }
    let docs = idx.num_docs();
    tracing::info!("search index: ready, {docs} documents, built in {secs} s");
    Ok(docs)
}

/// Re-index the infohashes of the oldest queued changes; returns how many log rows were done.
pub async fn apply_changes(pool: &PgPool, idx: &Arc<SearchIndex>) -> Result<usize> {
    let (ids, infohashes) = load::pending_changes(pool, APPLY_BATCH).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let docs = load::load_docs(pool, &infohashes).await?;
    let applied = ids.iter().copied().max().unwrap_or(0);
    {
        let idx = idx.clone();
        tokio::task::spawn_blocking(move || idx.replace(&infohashes, docs, applied))
            .await
            .map_err(join_err)??;
    }
    load::ack_changes(pool, &ids).await?;
    Ok(ids.len())
}

/// Builds the index when needed, then follows the change log until shutdown.
pub async fn index_loop(pool: PgPool, idx: Arc<SearchIndex>, mut shutdown: watch::Receiver<bool>) {
    loop {
        if !idx.is_ready() {
            let res = tokio::select! {
                res = build(&pool, &idx) => res,
                _ = stop_requested(&mut shutdown) => return,
            };
            if let Err(e) = res {
                tracing::warn!("search index build: {e}; retrying in 60 s");
                idx.set_error(&e);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => continue,
                    _ = stop_requested(&mut shutdown) => return,
                }
            }
        }
        let res = tokio::select! {
            res = apply_changes(&pool, &idx) => res,
            _ = stop_requested(&mut shutdown) => return,
        };
        let pause = match res {
            Ok(n) if n as i64 >= APPLY_BATCH => Duration::ZERO,
            Ok(_) => APPLY_EVERY,
            Err(e) => {
                tracing::warn!("search index update: {e}");
                Duration::from_secs(10)
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

/// With the index disabled nothing reads the change log: keep it empty (hourly).
pub async fn discard_loop(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    loop {
        match load::clear_changes(&pool).await {
            Ok(n) if n > 0 => tracing::debug!("search index off: dropped {n} queued changes"),
            Ok(_) => {}
            Err(e) => tracing::warn!("search changes: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}
