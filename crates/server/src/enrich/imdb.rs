//! IMDb non-commercial datasets (D40): conditional download, streamed import, atomic swap,
//! and the candidate lookups used by the matcher.
//!
//! Files (datasets.imdbws.com, refreshed daily upstream, here every
//! `MAGNETAR_IMDB_REFRESH_HOURS`): `title.basics`, `title.ratings`, `title.episode`, and
//! `title.akas` only with `MAGNETAR_IMDB_AKAS=true`. Each download sends `If-Modified-Since`
//! (the stored `Last-Modified`, else the file's mtime), so an unchanged file costs one 304.
//!
//! Import: ratings into memory, then title.basics is streamed (gzip → TSV → COPY text) into
//! staging tables `imdb_*_new` while two connections COPY titles and names concurrently;
//! episodes (and akas) follow. Indexes are built on the staging tables, then one transaction
//! swaps them in, so readers see the old dataset or the new one, never a mix. Licence:
//! personal non-commercial use only; nothing here is republished.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use magnetar_core::enrich::imdb_match::{Candidate, NameSource, TitleType, name_keys};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use tokio::sync::mpsc;

pub const BASE_URL: &str = "https://datasets.imdbws.com";
pub const BASICS: &str = "title.basics";
pub const RATINGS: &str = "title.ratings";
pub const EPISODE: &str = "title.episode";
pub const AKAS: &str = "title.akas";
/// Whole-download timeout (title.akas is ~450 MB).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3600);
/// COPY chunk size.
const CHUNK: usize = 1 << 20;
/// pg_advisory_lock key held during an import (a second process skips instead of racing).
const IMPORT_LOCK: i64 = 0x6d61_676e_696d_6462; // "magnimdb"

#[derive(Clone, Debug)]
pub struct ImdbConfig {
    pub dir: PathBuf,
    pub akas: bool,
    pub base_url: String,
    /// An import with fewer titles is refused (truncated download); [`MIN_TITLES`] normally.
    pub min_titles: u64,
}

/// title.basics had 1,563,990 kept titles on 2026-10-25.
pub const MIN_TITLES: u64 = 500_000;

impl ImdbConfig {
    pub fn datasets(&self) -> Vec<&'static str> {
        let mut v = vec![BASICS, RATINGS, EPISODE];
        if self.akas {
            v.push(AKAS);
        }
        v
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.tsv.gz"))
    }
}

/// Result of one conditional GET.
#[derive(Clone, Debug, Serialize)]
pub struct Fetched {
    pub name: String,
    /// `Last-Modified` of the local copy (from the server, or the file mtime).
    pub version: String,
    pub bytes: u64,
    /// false: 304 Not Modified (or offline with a local copy).
    pub downloaded: bool,
}

fn sidecar(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".last-modified");
    PathBuf::from(p)
}

/// The version string of a local file: its sidecar, else its mtime as an HTTP date.
fn local_version(path: &Path) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(sidecar(path))
        && !s.trim().is_empty()
    {
        return Some(s.trim().to_string());
    }
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(httpdate::fmt_http_date(mtime))
}

/// Conditional download of `<name>.tsv.gz` into `cfg.dir`. A network error with a local copy
/// present keeps the local copy (offline servers keep working).
pub async fn fetch(
    client: &reqwest::Client,
    cfg: &ImdbConfig,
    name: &str,
) -> anyhow::Result<Fetched> {
    std::fs::create_dir_all(&cfg.dir).with_context(|| format!("create {}", cfg.dir.display()))?;
    let path = cfg.path(name);
    let local = path.exists().then(|| local_version(&path)).flatten();
    let url = format!("{}/{name}.tsv.gz", cfg.base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if let Some(v) = &local {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, v.as_str());
    }
    let keep_local = |why: String| -> anyhow::Result<Fetched> {
        let Some(v) = local.clone() else {
            bail!("{url}: {why} and no local copy");
        };
        tracing::warn!("imdb: {url}: {why}; using the local copy");
        Ok(Fetched {
            name: name.into(),
            version: v,
            bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            downloaded: false,
        })
    };
    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return keep_local(e.to_string()),
    };
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED && local.is_some() {
        return Ok(Fetched {
            name: name.into(),
            version: local.unwrap_or_default(),
            bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            downloaded: false,
        });
    }
    if !resp.status().is_success() {
        return keep_local(format!("HTTP {}", resp.status()));
    }
    let last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let expected = resp.content_length();
    let part = path.with_extension("gz.part");
    let mut file = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("create {}", part.display()))?;
    let mut bytes = 0u64;
    let started = Instant::now();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                bytes += c.len() as u64;
                tokio::io::AsyncWriteExt::write_all(&mut file, &c).await?;
            }
            Ok(None) => break,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return keep_local(format!("download interrupted: {e}"));
            }
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    file.sync_all().await?;
    drop(file);
    if expected.is_some_and(|n| n != bytes) {
        let _ = tokio::fs::remove_file(&part).await;
        return keep_local(format!("short download ({bytes} of {expected:?} bytes)"));
    }
    tokio::fs::rename(&part, &path).await?;
    let version = match last_modified {
        Some(lm) => {
            std::fs::write(sidecar(&path), &lm)?;
            if let Ok(t) = httpdate::parse_http_date(&lm)
                && let Ok(f) = std::fs::File::options().write(true).open(&path)
            {
                let _ = f.set_modified(t);
            }
            lm
        }
        None => {
            let _ = std::fs::remove_file(sidecar(&path));
            httpdate::fmt_http_date(SystemTime::now())
        }
    };
    tracing::info!(
        "imdb: downloaded {name} ({bytes} bytes in {} s)",
        started.elapsed().as_secs()
    );
    Ok(Fetched {
        name: name.into(),
        version,
        bytes,
        downloaded: true,
    })
}

/// What one import did (logged, and printed by `magnetar-server enrich`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub titles: u64,
    pub names: u64,
    pub episodes: u64,
    pub akas: u64,
    pub media_refreshed: u64,
    pub seconds: f64,
    /// Total size of the imdb_* tables and their indexes after the swap.
    pub db_bytes: i64,
}

/// COPY text escaping (backslash, tab, newline, CR).
fn esc(out: &mut Vec<u8>, s: &str) {
    for b in s.bytes() {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            _ => out.push(b),
        }
    }
}

fn opt_num(out: &mut Vec<u8>, v: &str) {
    if v == "\\N" || v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        out.extend_from_slice(b"\\N");
    } else {
        out.extend_from_slice(v.as_bytes());
    }
}

fn tconst(s: &str) -> Option<u32> {
    s.strip_prefix("tt")?.parse().ok()
}

fn hash_key(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn gz_lines(
    path: &Path,
) -> anyhow::Result<std::io::Lines<BufReader<flate2::read::MultiGzDecoder<std::fs::File>>>> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut lines = BufReader::with_capacity(1 << 16, flate2::read::MultiGzDecoder::new(f)).lines();
    // Header row.
    lines.next().transpose()?;
    Ok(lines)
}

/// Sends full chunks to a COPY consumer from a blocking thread.
struct Sink {
    tx: mpsc::Sender<Vec<u8>>,
    buf: Vec<u8>,
    rows: u64,
}

impl Sink {
    fn new(tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(CHUNK + 4096),
            rows: 0,
        }
    }

    fn row_done(&mut self) -> anyhow::Result<()> {
        self.buf.push(b'\n');
        self.rows += 1;
        if self.buf.len() >= CHUNK {
            let full = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK + 4096));
            self.tx
                .blocking_send(full)
                .map_err(|_| anyhow::anyhow!("COPY consumer stopped"))?;
        }
        Ok(())
    }

    fn finish(mut self) -> anyhow::Result<u64> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.tx
                .blocking_send(rest)
                .map_err(|_| anyhow::anyhow!("COPY consumer stopped"))?;
        }
        Ok(self.rows)
    }
}

/// Run `COPY <table> FROM STDIN` fed by `rx` until the producer hangs up.
async fn copy_from(
    conn: &mut PgConnection,
    table: &'static str,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> anyhow::Result<u64> {
    let mut copy = conn
        .copy_in_raw(&format!("COPY {table} FROM STDIN"))
        .await
        .with_context(|| format!("COPY {table}"))?;
    while let Some(buf) = rx.recv().await {
        if let Err(e) = copy.send(buf).await {
            let _ = copy.abort("send failed").await;
            return Err(e).with_context(|| format!("COPY {table}"));
        }
    }
    Ok(copy.finish().await?)
}

/// tconst → (rating, votes).
fn read_ratings(path: &Path) -> anyhow::Result<HashMap<u32, (f32, u32)>> {
    let mut map = HashMap::with_capacity(1_800_000);
    for line in gz_lines(path)? {
        let line = line?;
        let mut it = line.split('\t');
        let (Some(id), Some(r), Some(v)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if let (Some(id), Ok(r), Ok(v)) = (tconst(id), r.parse(), v.parse()) {
            map.insert(id, (r, v));
        }
    }
    Ok(map)
}

/// Kept titles: id → (type, hashes of its primary/original name keys).
/// Two hashes are enough to skip most akas that only repeat the primary/original title.
type Kept = HashMap<u32, (TitleType, [u64; 2])>;

/// title.basics → imdb_titles_new + imdb_names_new rows.
fn produce_basics(
    path: &Path,
    ratings: &HashMap<u32, (f32, u32)>,
    titles: &mut Sink,
    names: &mut Sink,
) -> anyhow::Result<Kept> {
    let mut kept: Kept = HashMap::with_capacity(1_700_000);
    for line in gz_lines(path)? {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 {
            continue;
        }
        let (Some(id), Some(tt)) = (tconst(f[0]), TitleType::from_imdb(f[1])) else {
            continue;
        };
        if f[4] == "1" {
            continue; // isAdult
        }
        let primary = f[2];
        let original = (f[3] != "\\N" && f[3] != primary).then_some(f[3]);
        // id, title_type, title, original_title, year, end_year, runtime_min, genres, rating, votes
        let b = &mut titles.buf;
        b.extend_from_slice(format!("{id}\t{}\t", tt.code()).as_bytes());
        esc(b, primary);
        b.push(b'\t');
        match original {
            Some(o) => esc(b, o),
            None => b.extend_from_slice(b"\\N"),
        }
        b.push(b'\t');
        opt_num(b, f[5]);
        b.push(b'\t');
        opt_num(b, f[6]);
        b.push(b'\t');
        opt_num(b, f[7]);
        b.push(b'\t');
        if f[8] == "\\N" || f[8].is_empty() {
            b.extend_from_slice(b"{}");
        } else {
            b.push(b'{');
            for (i, g) in f[8].split(',').enumerate() {
                if i > 0 {
                    b.push(b',');
                }
                // Genre names are plain words; quote anyway for the array literal.
                b.push(b'"');
                esc(b, &g.replace(['"', '\\', '{', '}', ','], ""));
                b.push(b'"');
            }
            b.push(b'}');
        }
        b.push(b'\t');
        match ratings.get(&id) {
            Some((r, v)) => b.extend_from_slice(format!("{r}\t{v}").as_bytes()),
            None => b.extend_from_slice(b"\\N\t0"),
        }
        titles.row_done()?;

        let mut hashes = [0u64; 2];
        let mut seen: Vec<String> = Vec::with_capacity(4);
        for (src, t) in [
            (NameSource::Primary, Some(primary)),
            (NameSource::Original, original),
        ] {
            let Some(t) = t else { continue };
            for k in name_keys(t) {
                if seen.contains(&k) {
                    continue;
                }
                if let Some(slot) = hashes.iter_mut().find(|h| **h == 0) {
                    *slot = hash_key(&k);
                }
                esc(&mut names.buf, &k);
                names
                    .buf
                    .extend_from_slice(format!("\t{id}\t{}", src as i16).as_bytes());
                names.row_done()?;
                seen.push(k);
            }
        }
        kept.insert(id, (tt, hashes));
    }
    Ok(kept)
}

/// Episode record size in `imdb_episodes.data`.
pub const EPISODE_RECORD: usize = 9;

/// Packed record for one episode (see the migration).
fn episode_record(
    id: u32,
    parent: u32,
    season: Option<i16>,
    episode: Option<i16>,
) -> [u8; EPISODE_RECORD] {
    let mut r = [0u8; EPISODE_RECORD];
    r[0] = (id & 0xff) as u8;
    r[1..5].copy_from_slice(&parent.to_le_bytes());
    r[5..7].copy_from_slice(&season.unwrap_or(i16::MIN).to_le_bytes());
    r[7..9].copy_from_slice(&episode.unwrap_or(i16::MIN).to_le_bytes());
    r
}

/// Find an episode in a bucket's packed records: (parent, season, episode).
pub fn find_episode(data: &[u8], id: u32) -> Option<(u32, Option<i32>, Option<i32>)> {
    let low = (id & 0xff) as u8;
    data.as_chunks::<EPISODE_RECORD>()
        .0
        .iter()
        .find(|r| r[0] == low)
        .map(|r| {
            let parent = u32::from_le_bytes([r[1], r[2], r[3], r[4]]);
            let num = |a: u8, b: u8| {
                Some(i16::from_le_bytes([a, b]))
                    .filter(|n| *n != i16::MIN)
                    .map(i32::from)
            };
            (parent, num(r[5], r[6]), num(r[7], r[8]))
        })
}

/// title.episode → imdb_episodes_new (episodes of kept series only), packed by `id >> 8`. The
/// file is sorted as text (tt10… before tt2…), so buckets are collected first.
fn produce_episodes(path: &Path, kept: &Kept, out: &mut Sink) -> anyhow::Result<u64> {
    let mut buckets: HashMap<u32, Vec<u8>> = HashMap::with_capacity(200_000);
    let mut n = 0u64;
    for line in gz_lines(path)? {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        let (Some(id), Some(parent)) = (tconst(f[0]), tconst(f[1])) else {
            continue;
        };
        if !kept.get(&parent).is_some_and(|(t, _)| t.is_series()) {
            continue;
        }
        let rec = episode_record(id, parent, f[2].parse().ok(), f[3].trim_end().parse().ok());
        buckets.entry(id >> 8).or_default().extend_from_slice(&rec);
        n += 1;
    }
    let mut keys: Vec<u32> = buckets.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        let data = &buckets[&k];
        // bytea hex input: \\x + hex (the backslash is escaped for COPY text).
        out.buf.extend_from_slice(format!("{k}\t\\\\x").as_bytes());
        out.buf.extend_from_slice(hex::encode(data).as_bytes());
        out.row_done()?;
    }
    Ok(n)
}

/// title.akas → extra imdb_names_new rows (source 3), minus keys the title already has.
fn produce_akas(path: &Path, kept: &Kept, names: &mut Sink) -> anyhow::Result<()> {
    let mut last: Option<u32> = None;
    let mut seen: HashSet<u64> = HashSet::new();
    for line in gz_lines(path)? {
        let line = line?;
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        let Some(id) = tconst(f[0]) else { continue };
        let Some((_, own)) = kept.get(&id) else {
            continue;
        };
        if last != Some(id) {
            last = Some(id);
            seen.clear();
            seen.extend(own.iter().copied().filter(|h| *h != 0));
        }
        for k in name_keys(f[2]) {
            if seen.insert(hash_key(&k)) {
                esc(&mut names.buf, &k);
                names
                    .buf
                    .extend_from_slice(format!("\t{id}\t{}", NameSource::Aka as i16).as_bytes());
                names.row_done()?;
            }
        }
    }
    Ok(())
}

/// Full rebuild of the imdb_* tables from the local files, then the swap.
pub async fn import(
    pool: &PgPool,
    cfg: &ImdbConfig,
    versions: &[Fetched],
) -> anyhow::Result<ImportReport> {
    let started = Instant::now();
    let mut lock = pool.acquire().await?;
    // A cancelled import (shutdown) must not hand a connection holding the session lock back
    // to the pool: closing it releases the lock.
    lock.close_on_drop();
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(IMPORT_LOCK)
        .fetch_one(&mut *lock)
        .await?;
    if !got {
        bail!("another IMDb import is running");
    }
    let res = import_locked(pool, &mut lock, cfg, versions, started).await;
    if res.is_err() {
        let _ =
            sqlx::query("DROP TABLE IF EXISTS imdb_titles_new, imdb_names_new, imdb_episodes_new")
                .execute(pool)
                .await;
    }
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(IMPORT_LOCK)
        .execute(&mut *lock)
        .await;
    res
}

/// The import itself, on the lock connection plus one more while titles and names are copied
/// concurrently: at most 2 of the server's `store::MAX_CONNECTIONS` (the API, ingest and the
/// other daemons share the pool).
async fn import_locked(
    pool: &PgPool,
    conn: &mut PgConnection,
    cfg: &ImdbConfig,
    versions: &[Fetched],
    started: Instant,
) -> anyhow::Result<ImportReport> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS imdb_titles_new, imdb_names_new, imdb_episodes_new;
         CREATE UNLOGGED TABLE imdb_titles_new (LIKE imdb_titles INCLUDING DEFAULTS);
         CREATE UNLOGGED TABLE imdb_names_new (LIKE imdb_names INCLUDING DEFAULTS);
         CREATE UNLOGGED TABLE imdb_episodes_new (LIKE imdb_episodes INCLUDING DEFAULTS);",
    )
    .execute(&mut *conn)
    .await?;

    let basics = cfg.path(BASICS);
    let ratings_path = cfg.path(RATINGS);
    let episode = cfg.path(EPISODE);
    let akas = cfg.akas.then(|| cfg.path(AKAS));

    let (titles_tx, titles_rx) = mpsc::channel::<Vec<u8>>(8);
    let (names_tx, names_rx) = mpsc::channel::<Vec<u8>>(8);
    let (ep_tx, ep_rx) = mpsc::channel::<Vec<u8>>(8);
    let mut names_conn = pool.acquire().await?;
    names_conn.close_on_drop();
    let producer = tokio::task::spawn_blocking(move || -> anyhow::Result<(u64, u64, u64, u64)> {
        let t0 = Instant::now();
        let ratings = read_ratings(&ratings_path)?;
        tracing::info!(
            "imdb: {} ratings read in {} ms",
            ratings.len(),
            t0.elapsed().as_millis()
        );
        let mut titles = Sink::new(titles_tx);
        let mut names = Sink::new(names_tx);
        let kept = produce_basics(&basics, &ratings, &mut titles, &mut names)?;
        drop(ratings);
        let n_titles = titles.finish()?;
        let names_before = names.rows;
        tracing::info!(
            "imdb: {n_titles} titles streamed in {} ms",
            t0.elapsed().as_millis()
        );
        if let Some(akas) = akas {
            produce_akas(&akas, &kept, &mut names)?;
        }
        let n_akas = names.rows - names_before;
        let n_names = names.finish()?;
        let mut eps = Sink::new(ep_tx);
        let n_eps = produce_episodes(&episode, &kept, &mut eps)?;
        let buckets = eps.finish()?;
        tracing::info!("imdb: {n_eps} episodes in {buckets} buckets");
        Ok((n_titles, n_names, n_eps, n_akas))
    });
    // Titles and names are produced together, so their COPYs run together (lock connection +
    // one more, given back when the names, akas included, are done). Episodes are produced
    // after both (their senders are dropped then) and reuse the lock connection.
    let (c_titles, c_names) = tokio::join!(
        copy_from(&mut *conn, "imdb_titles_new", titles_rx),
        copy_from(&mut names_conn, "imdb_names_new", names_rx),
    );
    drop(names_conn);
    let c_eps = copy_from(&mut *conn, "imdb_episodes_new", ep_rx).await;
    let produced = producer.await.context("import thread")?;
    let copied = (c_titles, c_names, c_eps);
    let (n_titles, n_names, n_eps, n_akas) = produced?;
    let (c_titles, c_names, _) = (copied.0?, copied.1?, copied.2?);
    if (c_titles, c_names) != (n_titles, n_names) {
        bail!("COPY row counts differ: produced {n_titles}/{n_names}, copied {c_titles}/{c_names}");
    }
    if n_titles < cfg.min_titles {
        bail!("title.basics gave only {n_titles} titles; refusing to replace the dataset");
    }
    tracing::info!(
        "imdb: copied {n_titles} titles, {n_names} names ({n_akas} akas), {n_eps} episodes in {} s; indexing",
        started.elapsed().as_secs()
    );

    sqlx::raw_sql(
        "SET maintenance_work_mem = '128MB';
         ALTER TABLE imdb_titles_new ADD CONSTRAINT imdb_titles_new_pkey PRIMARY KEY (id);
         ALTER TABLE imdb_episodes_new ADD CONSTRAINT imdb_episodes_new_pkey PRIMARY KEY (bucket);
         CREATE INDEX imdb_names_new_norm ON imdb_names_new (norm);
         CREATE INDEX imdb_names_new_trgm ON imdb_names_new USING GIN (norm gin_trgm_ops);
         ANALYZE imdb_titles_new; ANALYZE imdb_names_new; ANALYZE imdb_episodes_new;
         RESET maintenance_work_mem;",
    )
    .execute(&mut *conn)
    .await?;

    let now = magnetar_core::now();
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    sqlx::raw_sql(
        "DROP TABLE imdb_titles;
         ALTER TABLE imdb_titles_new RENAME TO imdb_titles;
         ALTER TABLE imdb_titles RENAME CONSTRAINT imdb_titles_new_pkey TO imdb_titles_pkey;
         DROP TABLE imdb_names;
         ALTER TABLE imdb_names_new RENAME TO imdb_names;
         ALTER INDEX imdb_names_new_norm RENAME TO imdb_names_norm;
         ALTER INDEX imdb_names_new_trgm RENAME TO imdb_names_trgm;
         DROP TABLE imdb_episodes;
         ALTER TABLE imdb_episodes_new RENAME TO imdb_episodes;
         ALTER TABLE imdb_episodes RENAME CONSTRAINT imdb_episodes_new_pkey TO imdb_episodes_pkey;",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM imdb_datasets")
        .execute(&mut *tx)
        .await?;
    for f in versions {
        let rows = match f.name.as_str() {
            BASICS => n_titles,
            EPISODE => n_eps,
            AKAS => n_akas,
            _ => 0,
        };
        sqlx::query(
            "INSERT INTO imdb_datasets (name, last_modified, bytes, rows, imported_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&f.name)
        .bind(&f.version)
        .bind(i64::try_from(f.bytes).unwrap_or(i64::MAX))
        .bind(i64::try_from(rows).unwrap_or(i64::MAX))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    let refreshed = refresh_media(&mut tx, now).await?;
    tx.commit().await?;

    let db_bytes: i64 = sqlx::query_scalar(
        "SELECT (pg_total_relation_size('imdb_titles') + pg_total_relation_size('imdb_names')
                 + pg_total_relation_size('imdb_episodes'))::bigint",
    )
    .fetch_one(&mut *conn)
    .await?;
    let report = ImportReport {
        titles: n_titles,
        names: n_names,
        episodes: n_eps,
        akas: n_akas,
        media_refreshed: refreshed,
        seconds: started.elapsed().as_secs_f64(),
        db_bytes,
    };
    tracing::info!("imdb: import done: {report:?}");
    Ok(report)
}

/// Copy fresh IMDb fields onto existing media rows (only rows that changed). Rows of kind
/// `anime` that carry an anime id are D41's (it writes the same columns from its own dataset)
/// and are left alone.
async fn refresh_media(conn: &mut sqlx::PgConnection, now: i64) -> anyhow::Result<u64> {
    let r = sqlx::query(
        "UPDATE media m SET title = t.title, original_title = t.original_title,
                year = t.year, end_year = t.end_year, genres = t.genres, rating = t.rating,
                votes = t.votes, runtime_min = t.runtime_min,
                kind = CASE WHEN m.kind = 'anime' THEN m.kind
                            WHEN t.title_type IN (4, 5) THEN 'series' ELSE 'movie' END,
                updated_at = $1
         FROM imdb_titles t
         WHERE m.imdb ~ '^tt[0-9]{1,9}$' AND substr(m.imdb, 3)::int = t.id
           AND NOT (m.kind = 'anime'
                    AND (m.mal IS NOT NULL OR m.anilist IS NOT NULL OR m.anidb IS NOT NULL))
           AND (m.title, m.original_title, m.year, m.end_year, m.genres, m.rating, m.votes, m.runtime_min)
               IS DISTINCT FROM
               (t.title, t.original_title, t.year::int, t.end_year::int, t.genres, t.rating, t.votes, t.runtime_min)",
    )
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// When the current dataset was imported (None: never, or the tables are empty — e.g. after
/// a crash truncated the UNLOGGED tables).
pub async fn imported_at(pool: &PgPool) -> anyhow::Result<Option<i64>> {
    let at: Option<i64> = sqlx::query_scalar("SELECT max(imported_at) FROM imdb_datasets")
        .fetch_one(pool)
        .await?;
    if at.is_none() {
        return Ok(None);
    }
    let any: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM imdb_titles)")
        .fetch_one(pool)
        .await?;
    Ok(any.then_some(at).flatten())
}

/// Download what changed and re-import when any file changed, the akas setting changed, or the
/// tables are empty. Returns None when nothing had to be imported.
pub async fn refresh(
    pool: &PgPool,
    cfg: &ImdbConfig,
    force: bool,
) -> anyhow::Result<Option<ImportReport>> {
    let client = magnetar_core::http::client(DOWNLOAD_TIMEOUT)?;
    let mut fetched = Vec::new();
    for name in cfg.datasets() {
        fetched.push(fetch(&client, cfg, name).await?);
    }
    let stored: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, last_modified FROM imdb_datasets")
            .fetch_all(pool)
            .await?;
    let same = stored.len() == fetched.len()
        && fetched.iter().all(|f| {
            stored
                .iter()
                .any(|(n, v)| *n == f.name && v.as_deref() == Some(f.version.as_str()))
        });
    if same && !force && imported_at(pool).await?.is_some() {
        tracing::info!("imdb: datasets unchanged");
        return Ok(None);
    }
    import(pool, cfg, &fetched).await.map(Some)
}

#[derive(FromRow)]
struct CandRow {
    norm: String,
    source: i16,
    id: i32,
    title_type: i16,
    year: Option<i16>,
    end_year: Option<i16>,
    votes: i32,
}

/// Exact-name candidates for many keys at once: key → candidates (main_title = false; the
/// caller sets it per parsed name).
pub async fn candidates(
    pool: &PgPool,
    keys: &[String],
) -> anyhow::Result<HashMap<String, Vec<Candidate>>> {
    let mut out: HashMap<String, Vec<Candidate>> = HashMap::new();
    if keys.is_empty() {
        return Ok(out);
    }
    let rows: Vec<CandRow> = sqlx::query_as(
        "SELECT n.norm, n.source, t.id, t.title_type, t.year, t.end_year, t.votes
         FROM imdb_names n JOIN imdb_titles t ON t.id = n.id
         WHERE n.norm = ANY($1)",
    )
    .bind(keys)
    .fetch_all(pool)
    .await?;
    for r in rows {
        let Some(tt) = TitleType::from_code(r.title_type) else {
            continue;
        };
        let list = out.entry(r.norm).or_default();
        let c = Candidate {
            id: u32::try_from(r.id).unwrap_or(0),
            title_type: tt,
            year: r.year.map(i32::from),
            end_year: r.end_year.map(i32::from),
            votes: u32::try_from(r.votes).unwrap_or(0),
            source: NameSource::from_code(r.source),
            main_title: false,
        };
        // One entry per title and key (primary beats original beats aka).
        match list.iter_mut().find(|x| x.id == c.id) {
            Some(x) if (c.source as i16) < (x.source as i16) => *x = c,
            Some(_) => {}
            None => list.push(c),
        }
    }
    Ok(out)
}

#[derive(FromRow)]
struct FuzzyRow {
    sim: f32,
    source: i16,
    id: i32,
    title_type: i16,
    year: Option<i16>,
    end_year: Option<i16>,
    votes: i32,
}

/// pg_trgm candidates with similarity ≥ `min_sim` (best first, at most 20).
pub async fn fuzzy_candidates(
    pool: &PgPool,
    key: &str,
    min_sim: f32,
) -> anyhow::Result<Vec<(Candidate, f32)>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('pg_trgm.similarity_threshold', $1, true)")
        .bind(min_sim.to_string())
        .execute(&mut *tx)
        .await?;
    let rows: Vec<FuzzyRow> = sqlx::query_as(
        "SELECT similarity(n.norm, $1) AS sim, n.source, t.id, t.title_type, t.year, t.end_year, t.votes
         FROM imdb_names n JOIN imdb_titles t ON t.id = n.id
         WHERE n.norm % $1
         ORDER BY sim DESC, t.votes DESC LIMIT 20",
    )
    .bind(key)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some((
                Candidate {
                    id: u32::try_from(r.id).ok()?,
                    title_type: TitleType::from_code(r.title_type)?,
                    year: r.year.map(i32::from),
                    end_year: r.end_year.map(i32::from),
                    votes: u32::try_from(r.votes).unwrap_or(0),
                    source: NameSource::from_code(r.source),
                    main_title: true,
                },
                r.sim,
            ))
        })
        .collect())
}

/// One title's fields for a media row.
#[derive(Clone, Debug, FromRow)]
pub struct TitleRow {
    pub id: i32,
    pub title_type: i16,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i16>,
    pub end_year: Option<i16>,
    pub runtime_min: Option<i32>,
    pub genres: Vec<String>,
    pub rating: Option<f32>,
    pub votes: i32,
}

/// Look up a tconst; an episode resolves to (its series, season, episode).
pub async fn resolve_title(
    conn: &mut sqlx::PgConnection,
    id: u32,
) -> anyhow::Result<Option<(TitleRow, Option<i32>, Option<i32>)>> {
    let id = i32::try_from(id).unwrap_or(-1);
    let row: Option<TitleRow> = sqlx::query_as("SELECT * FROM imdb_titles WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(r) = row {
        return Ok(Some((r, None, None)));
    }
    let Ok(uid) = u32::try_from(id) else {
        return Ok(None);
    };
    let data: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT data FROM imdb_episodes WHERE bucket = $1")
            .bind(i32::try_from(uid >> 8).unwrap_or(-1))
            .fetch_optional(&mut *conn)
            .await?;
    let Some((parent, s, e)) = data.and_then(|d| find_episode(&d, uid)) else {
        return Ok(None);
    };
    let parent = i32::try_from(parent).unwrap_or(-1);
    let row: Option<TitleRow> = sqlx::query_as("SELECT * FROM imdb_titles WHERE id = $1")
        .bind(parent)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|r| (r, s, e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_escaping() {
        let mut b = Vec::new();
        esc(&mut b, "a\tb\\c\nd");
        assert_eq!(b, b"a\\tb\\\\c\\nd");
        let mut b = Vec::new();
        opt_num(&mut b, "\\N");
        opt_num(&mut b, "1999");
        opt_num(&mut b, "12a");
        assert_eq!(b, b"\\N1999\\N");
        assert_eq!(tconst("tt0133093"), Some(133093));
        assert!(tconst("nm0000206").is_none());
        let mut data = Vec::new();
        data.extend_from_slice(&episode_record(0x0012_3456, 944_947, Some(1), Some(2)));
        data.extend_from_slice(&episode_record(0x0012_3457, 944_947, None, None));
        assert_eq!(
            find_episode(&data, 0x0012_3456),
            Some((944_947, Some(1), Some(2)))
        );
        assert_eq!(
            find_episode(&data, 0x0012_3457),
            Some((944_947, None, None))
        );
        assert_eq!(find_episode(&data, 0x0012_3458), None);
    }

    /// Real conditional GET against datasets.imdbws.com (title.ratings, ~9 MB).
    #[tokio::test]
    #[ignore = "network"]
    async fn conditional_download() {
        magnetar_core::http::install_crypto_provider();
        let dir =
            std::env::temp_dir().join(format!("magnetar-fetch-{:016x}", rand::random::<u64>()));
        let cfg = ImdbConfig {
            dir: dir.clone(),
            akas: false,
            base_url: BASE_URL.into(),
            min_titles: MIN_TITLES,
        };
        let client = magnetar_core::http::client(DOWNLOAD_TIMEOUT).unwrap();
        let first = fetch(&client, &cfg, RATINGS).await.unwrap();
        assert!(first.downloaded && first.bytes > 1_000_000, "{first:?}");
        let again = fetch(&client, &cfg, RATINGS).await.unwrap();
        assert!(!again.downloaded, "second fetch must be a 304: {again:?}");
        assert_eq!(again.version, first.version);
        assert!(read_ratings(&cfg.path(RATINGS)).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_dir_all(dir);
    }
}
