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
  from the media link shown to users (confidence ≥ 0.5, D40; `year` is the media year, else the
  year in the name), and since `SCHEMA_VERSION` 2 the release attributes of `torrent_attrs`
  (D44, docs/FILTERS.md): `f_language`, `f_subtitles`, `f_resolution`, `f_source`, `f_codec`,
  `f_hdr`, `f_audio`, `f_channels`, `f_edition`, `f_format`, `f_group`; plus fast `year` and the
  stored `attrs` JSON returned in every hit;
- since `SCHEMA_VERSION` 3 (D52, D70), fast `adult` (1 when the canonical category is `porn` or
  a `dht_torrents` row of the infohash is classified `porn`). The version bump rebuilds the
  index once after the upgrade (35 s on the production copy for version 2).
- since `SCHEMA_VERSION` 4 (D51, D73), DHT torrents: a document per infohash with a
  `dht_torrents` row and no unblocked event (empty `event_id`/`pubkey`, trust 0), the DHT title
  and files on mixed documents, stored `origin` and facet `f_origin` (`nostr`, `dht`), and
  `eligible_trust` lifted to 1000 when a DHT row exists, so DHT results pass any `min_trust`.
  Filter `SearchQuery.origins` / `origin=nostr,dht`. Rebuild: 48 s on the production copy.

## Lifecycle

- **Open**: `serve` opens the index, or creates an empty one when the directory is missing,
  unreadable (also a missing or damaged segment file), built from another database (the id the
  migration stores in `settings.search.index_id`) or of another `SCHEMA_VERSION`. It also
  rebuilds a complete index that is out of step with the change log: each commit records the
  highest log id it covers, each acknowledgement stores the highest id in
  `meta.search_applied`, and an index behind `search_applied` (its directory restored from an
  older backup, or a second index following the same log) or ahead of the log's sequence (the
  database restored from an older `pg_dump`) would otherwise miss changes for good. Only an
  empty directory or one holding Tantivy files (`meta.json`, `.managed.json`) is ever wiped: a
  `MAGNETAR_SEARCH_INDEX_DIR` pointing at other data makes the index fail to open (searches
  stay on Postgres) instead of deleting it. Another process holding the directory's writer
  lock is an error too, never a reason to wipe.
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
  is in max(50, documents / 10 000) titles (`complete`, `french`: expanding those cost tens of
  milliseconds at 3 M documents and only added near-misses). A fuzzy match scores below an exact
  one, so a title holding the typo itself ranks first.
- Documents whose title holds every word get a flat +10 000, so title hits always rank above
  file-only and description-only hits; ties go to the newest.
- Adult documents are left out unless `include_adult` is set or `categories` selects `porn`
  (D52; query string `adult=1`; the SQL fallback tests the canonical category plus an `EXISTS`
  on `dht_torrents`, so both paths agree, tested in `tests/search_index.rs`).
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
  the canonical event. `rank` is the BM25 score (+10 000), not a 0–1 value. The fallback's rows
  include DHT rows (`torrent_rows`, D73): when the Nostr event is below `min_trust` it shows the
  DHT row's title, and a category filter also matches the DHT row's category.

## Counts

`SearchPage.total_is_lower_bound` (serde default false, so older servers and the phone's local
search read as exact): the index counts exactly; the Postgres fallback stops counting at 1000
and then sends `1000` with the flag, which the app shows as "1,000+".

## Facets

`GET /api/v1/facets?field=<name>&size=<n>&<search parameters>` → `FacetsResponse { field,
total, counts: [{value, count}] }`, most frequent first, over the results of the search
parameters (same parsing as `/api/v1/search`). Names: `category`, `category_path`,
`media_kind`, `genre`, `year`, and the D44 attributes `language`, `subtitles`, `resolution`,
`source`, `codec`, `hdr`, `audio`, `channels`, `edition`, `format`, `group`. 400 for an unknown
name, 503 `search_index_unavailable` while the index is disabled or building (older servers:
404), so the app hides facet UI then. `POST /api/v1/facets` (D44) counts several at once over a
JSON `SearchQuery`; every facet is counted without its own filter (docs/FILTERS.md).

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
| Full build | 3 236 660 documents in 221–269 s (12 000–14 700 docs/s; 4 document queries in flight, 4 writer threads); production size (462 k) ≈ 35 s |
| Index on disk | 2.2 GB, 32 segments (≈ 0.7 KiB per infohash; production size ≈ 320 MB) |
| Memory | process opening the built index: 38 MB anonymous after the query run, plus 655 MB of mapped index pages (page cache, reclaimable); while building: 1.2 GB peak RSS, of which ~680 MB anonymous (writer arenas, merges, row batches) |
| Update | one changed torrent → searchable 0.2–0.35 s after its commit (log read + reload + commit + reader reload; 1.2 s once, under merge load right after the build), plus up to 1 s of polling |
| Throughput | 8 concurrent clients, 296 queries in 7.2–7.5 s (40–41 queries/s) |

Latency, index already built (`MAGNETAR_BENCH_INDEX_DIR` reused, so the process did not build;
ms):

| query | p50 | p95 | max | total |
|---|---|---|---|---|
| browse `""` (date) | 15.6 | 19.9 | 20.4 | 3 236 660 |
| 1 letter `a` / `e` | 26.1 / 5.2 | 29.1 / 5.8 | 30.1 / 5.9 | 2 120 750 / 283 987 |
| 2 letters `th` / `le` | 1.7 / 18.9 | 2.3 / 20.9 | 2.3 / 21.2 | 27 398 / 1 241 386 |
| 3-letter prefix `the` / `one` | 31.8 / 9.1 | 36.6 / 10.8 | 40.6 / 11.2 | 1 005 264 / 135 623 |
| common `1080p` | 51.6 | 56.0 | 59.1 | 1 719 046 |
| common `french` / `multi` / `x265` | 26.9 / 28.8 / 17.8 | 28.7 / 30.7 / 21.0 | 28.8 / 30.8 / 23.8 | 808 053 / 813 015 / 391 964 |
| rare `chernobyl` / `documentaire` (both expanded with distance 2) | 49.8 / 49.4 | 54.2 / 50.4 | 61.8 / 50.7 | 840 / 50 876 |
| `the lord of the rings` / `game of thrones s01` / `lord rings 1080p` | 13.9 / 15.4 / 25.0 | 14.5 / 15.8 / 27.9 | 14.7 / 15.9 / 28.1 | 1 596 / 259 / 813 |
| typo `interstelar` / `matirx` / `ubuntoo` | 49.3 / 21.3 / 12.0 | 52.2 / 21.8 / 12.4 | 53.2 / 21.9 / 12.5 | 210 / 448 / 0 |
| 9 words `the lord of the rings complete integrale 1080p multi` | 27.2 | 29.4 | 33.6 | 0 |
| sorts: date, size, seeders × asc/desc, with `french` and browse (12 queries) | 18.0–42.7 | 19.3–54.0 | 20.2–57.5 | |
| filters (categories, ≥ 1 GiB, hide dead, ≥ 5 seeders, 1 year) `1080p` / browse | 61.6 / 34.5 | 69.7 / 40.2 | 71.3 / 41.5 | 39 253 / 47 887 |
| `min_trust` 20, browse / seeders `french` | 20.8 / 31.7 | 22.2 / 38.2 | 22.5 / 40.5 | 943 999 / 274 725 |
| page 10 (offset 180) | 18.1 | 21.6 | 21.7 | |
| **all 37 queries × 25 runs** | **26.2** | **51.6** | **71.3** | |
| facets `category` / `genre` (`french`) / `year`, one run | 11.7 / 20.0 / 18.9 | | | |

In the run that built the index in the same process (merges still running): p50 28.1 ms, p95
57.4 ms, max 76.0 ms. Before the merge-policy cap, the fuzzy gating and the matched-file
changes, the same set had p95 103–116 ms and maxima of 220 ms (the 9-word query, two words
expanded at distance 2 over titles and file names) and 139 ms (size sort over `french`, which
spent its time reading 5000-file lists of description-only hits for the matched-file line).

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
