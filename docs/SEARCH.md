# Search index (ARCHITECTURE.md D43)

The server answers searches from an embedded [Tantivy](https://github.com/quickwit-oss/tantivy)
index (crate `tantivy` =0.27.0, MIT) kept next to its state, instead of the Postgres query of D11.
Postgres stays the source of truth: the index is derived data that can be deleted at any time.
The phone is unchanged: it searches its SQLite cache (D12) offline, and the server through the API.

## Layout

| What | Where |
|---|---|
| Index files | `MAGNETAR_SEARCH_INDEX_DIR`, default `<state dir>/search-index` (docs/CONFIG.md) |
| Schema, analyzer, facets | `crates/server/src/search_index/schema.rs` (`SCHEMA_VERSION`) |
| Text → query | `search_index/query.rs` |
| Filters, sorts, counts, facet counts | `search_index/collect.rs` (one pass over the matches) |
| Postgres side (documents, change log) | `search_index/load.rs`, migration `20261028000000_search_index.sql` |
| Build / update loop, search entry points | `search_index/mod.rs` |

## Documents

One document per infohash, built from its events minus those of blocked publishers
(`local_overrides`), like the SQL search: canonical event = highest trust, then oldest, then
lowest id (D6). Fields:

- text (analyzer: split on non-alphanumerics, lowercase, ASCII folding, no stop words, no
  stemming): `title` (every distinct title of the infohash's events, canonical first, stored),
  `files` (first 16 KiB of `files_text`), `description` (first 2000 characters);
- stored + fast: `created_at`, `total_size`, `file_count`, `trust` (canonical), `pow_bits`,
  `seeders`, `leechers`, `best_peers`, `expires_at`, `category`, `media_id`, `media_year`,
  `media_rating`; fast only: `eligible_trust` (best publisher trust, 1000 for a locally trusted
  publisher); stored: `publisher_trust` (one per publisher), `infohash`, `event_id`, `pubkey`,
  `source_url`, `media_title`, `media_kind`;
- facets (keyword, fast): `f_category`, `f_category_path`, `f_media_kind`, `f_genre`, `f_year`
  from the media link shown to users (confidence ≥ 0.5, D40). STAGE 2 adds parsed attributes
  (language, resolution, source, codec, hdr, …) as more `FACETS` entries and a
  `SCHEMA_VERSION` bump.

## Lifecycle

- **Open**: `serve` opens the index, or creates an empty one when the directory is missing,
  unreadable, built from another database (the id the migration stores in
  `settings.search.index_id`) or of another `SCHEMA_VERSION`.
- **Build**: in the background, walking `torrent_infohashes` in byte order, 4 queries of 2000
  infohashes in flight, documents added on blocking threads (`spawn_blocking`), never on the
  async runtime. Every 200 000 infohashes it commits with the keyset cursor in the commit
  payload, so a restart resumes instead of starting over. Searches use Postgres until the build
  is complete; `/api/v1/stats` shows `search_index.build_done` / `build_total`.
- **Updates**: statement-level triggers append the infohashes touched by any write to
  `torrents`, `liveness`, `torrent_media`, `media` (only when kind, title, year, rating or genres
  change) and `local_overrides` to `search_changes`, an append-only log. The index loop reads up
  to 5000 log rows about every second, reloads those infohashes from Postgres, replaces their
  documents (absent = deleted) in one commit, then deletes exactly the log ids it read. Chosen
  over a dirty set with a primary key (writers would wait on each other's row locks and could
  deadlock across tables) and over polling `updated_at` columns (most write paths, including
  the enrichers' raw SQL, would have to remember to set them). A log row committed late with a
  lower id is simply read on the next pass. Changes made while no server runs (`crawl-once`,
  `enrich`) wait in the log.
- **Segments**: a merge policy caps segments at 1/16 of the index (at least 200 000
  documents), so broad queries fan out over the search threads (CPUs − 2, at most 16). Tantivy's
  default policy had merged the 3.2 M benchmark into segments of 1.04 M, 995 k and 684 k
  documents, and the largest bounded every broad query (`1080p` 65–82 ms; capped: 39–47 ms).
- **Disabled** (`MAGNETAR_SEARCH_INDEX=false`): the log is emptied hourly and an existing index
  directory removed, since it would be stale when re-enabled.

## Query

- Every word must match (title, file names or description). Per word: exact term with BM25,
  boosts title 3 > files 1.5 > description 0.6; the last word also as a prefix from 3 letters
  (constant score; descriptions from 4); fuzzy matching (Levenshtein, transposition = 1) at
  distance 1 from 5 letters and 2 from 8, on titles only, never on words containing digits
  (`s01e02` must not find `s01e01`, nor `2019` find `2018`), and not for a word already found as
  is in 50 titles or more (`complete`, `french`: expanding those cost tens of milliseconds at
  3 M documents and only added near-misses).
- Documents whose title holds every word get a flat +10 000, so title hits always rank above
  file-only and description-only hits; ties go to the newest.
- Filters (`categories`, size range, `since`/`until`, `min_trust` incl. local trust overrides,
  `hide_dead`, `min_seeders`, media tokens such as `imdb:tt…` resolved to a media id through
  Postgres) and sorts (relevance, date, size, seeders, both directions, unknown last) run in one
  collector over fast columns; it counts every match, so `total` is exact on the index path.
- `matched_file`: for hits whose title does not hold every word and that matched a word in a file
  name (one posting seek per hit), the file whose name matches the words best: exact > prefix >
  fuzzy per word, then the shorter path, among the first 1000 paths (one Postgres query per page,
  scored on a blocking thread).
- Differences from the SQL search, by design: date, size and category bounds apply to the
  canonical event (what the result shows), not to any event of the infohash; when the canonical
  event is below `min_trust` but another publisher is trusted locally, the result still shows
  the canonical event. `rank` is the BM25 score (+10 000), not a 0–1 value.

## Counts

`SearchPage.total_is_lower_bound` (serde default false, so older servers and the phone's local
search read as exact): the index counts exactly; the Postgres fallback stops counting at 1000
and then sends `1000` with the flag, which the app shows as "1,000+".

## Facets

`GET /api/v1/facets?field=<name>&size=<n>&<search parameters>` → `FacetsResponse { field,
total, counts: [{value, count}] }`, most frequent first, over the results of the search
parameters (same parsing as `/api/v1/search`). Names: `category`, `category_path`,
`media_kind`, `genre`, `year`. 400 for an unknown name, 503 `search_index_unavailable` while the
index is disabled or building (older servers: 404), so the app hides facet UI then.

## Stats counters

`/api/v1/stats` counted `DISTINCT infohash` and `DISTINCT pubkey` over `torrents` (wide rows:
3 GB for 466 k events; 14 s cold on the production laptop), and the crawler's publisher ranking
grouped the same table at every start. Both now read trigger-kept tables: `torrent_infohashes`
(infohash → events), `publisher_counts` (pubkey → torrents, indexed by count) and
`meta.infohash_count`, filled once by the migration.

## Benchmark (2026-10-26, this laptop: 12 threads, NVMe, Postgres 18 in docker)

Data: `scripts/bench-search-db.sh 7` copied the production database (466 870 events, 462 380
infohashes, 34 525 liveness rows, 131 003 media links; read-only `COPY … TO STDOUT`) into
`mag_night_bench` and inflated it ×7 with derived infohashes and release-name variations of every
title (resolution, codec, language, source, group, year), shifted dates and sizes: 3 268 090
events, **3 236 660 infohashes**, 18 GB in Postgres. Measured with `tests/search_bench.rs`
(`--release`), times taken around `PgStore::search` (index query + media token lookup +
matched-file query: what the API handler waits for), 25 runs per query after one warm-up run.

| | |
|---|---|
| Full build | 3 236 660 documents in 269 s (12 000 docs/s; 4 document queries in flight, 4 writer threads); production size (462 k) ≈ 40 s |
| Index on disk | 2.2 GB (≈ 0.7 KiB per infohash; production size ≈ 320 MB) |
| Memory | process opening the built index: 38 MB anonymous after the query run, plus 587 MB of mapped index pages (page cache, reclaimable); while building: 1.2 GB peak RSS, of which ~670 MB anonymous (writer arenas, merges, row batches) |
| Update | one changed torrent → searchable 230–350 ms after the trigger (commit + reader reload), plus up to 1 s of polling |
| Throughput | 8 concurrent clients, 296 queries in 7.1–7.4 s (40–42 queries/s) |

Latency, index already built (second run; ms):

| query | p50 | p95 | max | total |
|---|---|---|---|---|
| browse `""` (date) | 17.8 | 18.8 | 19.0 | 3 236 660 |
| 1 letter `a` / `e` | 22.5 / 4.2 | 23.7 / 5.3 | 25.9 / 5.6 | 2 120 750 / 283 987 |
| 2 letters `th` / `le` | 1.3 / 15.4 | 1.7 / 22.1 | 1.7 / 23.6 | 27 398 / 1 241 386 |
| 3-letter prefix `the` / `one` | 23.0 / 7.7 | 25.9 / 8.0 | 26.4 / 8.2 | 1 005 264 / 135 623 |
| common `1080p` | 39.1 | 41.5 | 47.2 | 1 719 046 |
| common `french` / `multi` / `x265` | 21.0 / 22.4 / 13.4 | 24.0 / 24.5 / 16.5 | 24.3 / 25.5 / 16.6 | 808 053 / 813 015 / 391 964 |
| rare `chernobyl` / `documentaire` | 5.2 / 7.1 | 5.9 / 7.5 | 6.0 / 7.8 | 329 / 50 764 |
| `the lord of the rings` / `game of thrones s01` / `lord rings 1080p` | 10.8 / 11.7 / 18.0 | 11.6 / 12.8 / 19.4 | 12.1 / 13.4 / 20.1 | 1 596 / 259 / 813 |
| typo `interstelar` / `matirx` / `ubuntoo` | 35.0 / 16.1 / 9.8 | 39.0 / 18.5 / 10.2 | 42.0 / 18.5 / 10.6 | 210 / 448 / 0 |
| 9 words `the lord of the rings complete integrale 1080p multi` | 19.7 | 20.8 | 20.8 | 0 |
| sorts: date, size, seeders × asc/desc, with `french` and browse (12 queries) | 17.8–37.0 | 19.8–39.0 | 22.9–40.4 | |
| filters (categories, ≥ 1 GiB, hide dead, ≥ 5 seeders, 1 year) `1080p` / browse | 60.9 / 31.2 | 63.2 / 35.6 | 81.2 / 36.1 | 39 152 / 47 770 |
| `min_trust` 20, browse / seeders `french` | 21.1 / 37.5 | 23.7 / 40.5 | 26.5 / 40.8 | 943 999 / 274 725 |
| page 10 (offset 180) | 30.3 | 33.2 | 33.3 | |
| **all 37 queries × 25 runs** | **21.8** | **39.0** | **81.2** | |
| facets `category` / `genre` (`french`) / `year`, one run | 16.7 / 24.3 / 26.9 | | | |

The first run (right after the build in the same process) measured p50 26 ms, p95 53 ms,
max 76 ms. Before the merge-policy cap and the fuzzy gating, the same set had p95 103 ms and a
max of 220 ms (the 9-word query) and 139 ms (size sort over `french`, which also spent its time
reading 5000-file lists for the matched-file line; now only hits that matched a file name fetch
at most 1000 paths).

The Postgres search on the same 3.2 M rows (fallback path, count capped at 1000, one run each):
`a` 33.9 s, `1080p` 129 s, `the lord of the rings` 2.2 s, `interstelar` 0.7 s, seeders sort over
`french` 138 s.

Reproduce:

```sh
cargo build --release -p magnetar-server
scripts/bench-search-db.sh 7          # ~35 min, prints MAGNETAR_BENCH_DATABASE_URL
MAGNETAR_BENCH_DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/mag_night_bench \
MAGNETAR_BENCH_INDEX_DIR=$PWD/target/bench-index \
  cargo test --release -p magnetar-server --test search_bench -- --ignored --nocapture
```

`tests/search_index.rs::latency_bounds_in_memory` runs the same query mix over 20 000 synthetic
documents in every `cargo test` (bound 1.5 s per query in debug builds, 150 ms in release: it
only catches pathologies such as an unbounded prefix expansion).
