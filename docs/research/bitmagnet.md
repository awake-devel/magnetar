# bitmagnet: architecture, crawler internals, classifier, schema, APIs, licence

Research date: 2026-10-25. Source read: https://github.com/bitmagnet-io/bitmagnet at
`e31b30d7351c269dc801876f92ebc59d31fee545` (2026-05-21, also tag `v0.10.1-beta.1`, a pre-release).
The latest stable release is `v0.10.0` (`4d4042a`, 2025-03-02). Both were checked with
`git ls-remote` and `gh release list` (fact-checked).

Every `path:line` is relative to the repo root at `e31b30d`:
`https://github.com/bitmagnet-io/bitmagnet/blob/e31b30d/<path>#L<line>`.
Project state: 4,434 stars, 98 open issues, not archived; about 12 commits between
`v0.10.0` and `e31b30d`, so the project is in maintenance mode. The `next` (`074dae5`) and
`dev` branches were not read.

Nothing from the repo was built or run. All findings come from reading code, and every
figure marked "FAQ" is a vendor claim, not a measurement.

Related: [dht-crawling.md](dht-crawling.md), [metadata-enrichment.md](metadata-enrichment.md).

---

## 0. TL;DR

- **Licence:** MIT. Two exceptions matter for Magnetar (also MIT):
  1. Code copied from anacrolix/dht, missinggo and torrent is **MPL-2.0**
     (`licenses/LICENSE_anacrolix`), a file-level copyleft. There is also a
     `licenses/LICENSE_alexliesenfeld`.
  2. `internal/protocol/metainfo/metainforequester/requester.go` (the BEP 9 fetcher) is
     **nearly identical** to magnetico's `cmd/magneticod/bittorrent/metadata/leech.go`,
     and magnetico is **AGPL-3.0** (`COPYING` at `boramalper/magnetico@5546a94`).
     bitmagnet acknowledges magnetico in prose (`bitmagnet.io/guides/tech-stack.md:59`:
     "cobbled together from the now archived magnetico project") but ships **no magnetico
     licence notice** in `licenses/` or `LICENSE`. **Do not port or translate this file.**
- **Design ideas worth copying:** bounded-channel pipeline with a semaphore per stage; BEP 51
  as the only infohash source; a stable Bloom filter for seen hashes; one batched SQL triage
  (fetch / rescrape / skip); batched writes plus a classifier job in the same transaction.
- **Do not copy:** 60 s re-sample instead of the BEP 51 interval; no global outbound cap;
  non-BEP 42 IDs; non-rotating token secret; no HTTP auth.
- **Off-DHT metadata:** TMDB only (movies, TV). Shared hard-coded API key by default.
- **Live run:** blocked by the permission classifier, so **no measured numbers exist** (§10).

---

## 1. Architecture

### 1.1 Processes and workers
- One Go binary on uber-go/fx (`go.mod`: `go.uber.org/fx v1.23.0`). Start with
  `bitmagnet worker run --all` or `--keys=...` (`internal/app/cmd/workercmd/command.go`).
- Three workers:
  - `http_server` (`internal/httpserver/server.go:35-36`): Gin; web UI, `/graphql`,
    `/torznab/*`, `/import`, `/metrics`, `/debug/pprof/*`, `/status`.
  - `queue_server` (`internal/queue/server/factory.go:32-33`): Postgres-backed job queue.
  - `dht_crawler` (`internal/dhtcrawler/factory.go:60-61`): crawler pipeline plus the DHT UDP
    server (started lazily).
- Several instances can share one database (`bitmagnet.io/faq.md`), relying on `SKIP LOCKED`.

### 1.2 How components talk
- **Inside the crawler:** Go channels only.
  - `BufferedConcurrentChannel(capacity, concurrency)`: buffered channel plus
    `semaphore.Weighted`, one goroutine per item (`internal/concurrency/buffered_concurrent_channel.go`).
  - `BatchingChannel(capacity, maxBatchSize, maxWaitTime)` (`internal/concurrency/batching_channel.go:21`).
- **Crawler to classifier:** through Postgres. The crawler writes `torrents`, `torrent_files`,
  `torrents_torrent_sources`, and in the same transaction a `queue_jobs` row (queue
  `process_torrent`, up to 100 infohashes), delayed 1 minute so the seeders/leechers scrape can
  land first (`internal/dhtcrawler/persist.go:40-57, 99-131, 214`).
- **Queue server** (`internal/queue/server/server.go`, adapted from acaloiaro/neoq,
  `internal/queue/doc.go:1`):
  - Polls; the `LISTEN/NOTIFY` code is commented out.
  - Each handler opens a transaction, picks the next `pending`/`retry` job with
    `run_after <= now()` via `FOR UPDATE SKIP LOCKED`, runs the job **inside that transaction**,
    then updates its status.
  - Defaults: `CheckInterval` 30 s, concurrency NumCPU (`internal/queue/handler/handler.go`).
    `process_torrent` overrides to concurrency 1, 10-minute timeout
    (`internal/processor/queue/handler.go:42-43`), max 2 retries (`internal/processor/message.go:33`).
  - GC every 10 min (`internal/queue/server/factory.go:57`).
- **Processor** (`internal/processor/processor.go`): loads torrents with files, hints, sources
  and existing `torrent_contents`; runs the CEL/YAML classifier, one goroutine per torrent,
  capped by `classifier.concurrency` = 10 (`internal/classifier/config.go`); writes
  `torrent_contents`, tags and deletions; re-enqueues failures.
- **Blocklist:** stable Bloom filter stored as a Postgres large object in `bloom_filters`,
  key `blocked_torrents` (`internal/blocking/manager.go:90,113`;
  `migrations/00020_bloom_filters_large_object.sql`).

---

## 2. DHT crawler internals

`internal/dhtcrawler/crawler.go:61-81` starts 15 goroutines (lines 65-79).

### 2.1 Pipeline (default ScalingFactor S = 10)
Defaults from `factory.go:84-121`, `config.go:27-36`, `discovered_nodes.go:26-31` (fact-checked:
concurrencies 10/100/100/200/200/400; buffer capacities 10/100/100/100/100/100).

| Stage | Capacity / concurrency | What it does |
|---|---|---|
| discoveredNodes (batching) | cap 1000, batch 10, wait 10 ms | Nodes from find_node, sample and get_peers replies, **and senders of incoming queries**. Drops IPs already in the table; pushes each to whichever of find_node, sample or ping accepts first (`discovered_nodes.go:33-60`). |
| nodesForPing | 10 / 10 | Pings unless the node answered in the last 15 min; drops on failure or ID mismatch (`ping.go:13-46`). Every 10 s, nodes silent for 15 min are re-queued (`ping.go:51-66`). |
| nodesForFindNode | 100 / 100 | `find_node` with a random target that changes every 10 s (`crawler.go:116-124`). Feeder takes the 10 oldest nodes once per second (`find_node.go:11-25`). |
| nodesForSampleInfoHashes | 100 / 100 | BEP 51 `sample_infohashes`; feeder takes up to 60 candidates/s (`sample_infohashes.go:11-25`); samples go through the Bloom filter to triage (`:27-60`). |
| infoHashTriage (batching) | cap 100, batch 1000, wait 20 s | Removes blocked hashes; one `SELECT … LEFT JOIN torrents_torrent_sources(source='dht') WHERE info_hash IN (…)` (`infohash_triage.go:22-107`). Unknown / no file info / stale file count → getPeers. Known with missing seeders/leechers or older than 30 days → scrape. Else drop. |
| getPeers | 100 / 200 | `get_peers` **only to the node that sampled the hash** (no iterative lookup). No `values` → abandon (`get_peers.go`). |
| requestMetaInfo | 100 / 400 | BEP 9 against each returned peer **in turn**, first success wins (`request_meta_info.go:13-58`). Banning checks: name ≥ 8 chars, size ≥ 1024 B, valid UTF-8 (`internal/protocol/metainfo/banning/checker.go:25-27`); failures go to the blocklist. |
| persistTorrents (batching) | cap 1000, batch 1000, wait 1 min | One transaction: torrents, `torrent_files` (≤ SaveFilesThreshold = 100, else `over_threshold`), sources, optional pieces (off), queue jobs (`persist.go:20-148`). Then to scrape. |
| scrape | 100 / 200 | BEP 33 `get_peers scrape=1` to the same single node; seeders/leechers = `ApproximatedSize()` of BFsd/BFpe (`scrape.go`; `persist.go:274-275`). |
| persistSources (batching) | cap 1000, batch 1000, wait 1 min | Upserts seeders, leechers, updated_at, guarded by `EXISTS(torrent)` (`persist.go:218-269`). |

- **Seen-hash filter:** `boom.NewStableBloomFilter(10_000_000, 2, 0.001)`, in memory only
  (`factory.go:121`). It forgets slowly, so hashes are re-triaged eventually. Memory ≈ 2.5 MB
  (UNVERIFIED estimate from constructor arguments).
- **Bootstrap nodes** (`config.go:39-46`): router.utorrent.com:6881, router.bittorrent.com:6881,
  dht.transmissionbt.com:6881, dht.aelitis.com:6881, router.silotis.us:6881,
  dht.libtorrent.org:25401. Re-pushed every **10 min, hard-coded** at `factory.go:84`; the
  `dht_crawler.reseed_bootstrap_nodes_interval` setting (default 1 min, `config.go:14,31`) is
  **never read** (fact-checked).

### 2.2 Routing table
- Binary-tree variant of k-buckets (`internal/protocol/dht/ktable/btree/btree.go:8-11`),
  **K = 80** for nodes and for hashes (`ktable/factory.go:28-31`); BEP 5 uses K = 8.
  Buckets split only near our own ID (`btree/node.go:51-53`).
- Sample candidate: BEP 51 support not known "no", next sample time passed, last response
  > 5 s ago (`ktable/node.go:151-158`).

### 2.3 Node ID
- 12 random bytes + ASCII suffix `-BM0001-` (`internal/protocol/id.go:30-50`); provided by
  `dhtfx/module.go` via `RandomNodeIDWithClientSuffix`. New on every start, never persisted,
  **not BEP 42**.
- BEP 9 peer ID: random with `-BM0001-` as a **prefix** (`id.go:221-231`).

### 2.4 BEP 51 interval handling
- `sample_infohashes.go:62-66`: `if len(discoveredHashes) > 0 && interval > 300 { interval = 60 }`
  ("most nodes request a 6 hour backoff time(!)"). This ignores BEP 51's "should not send a new
  sampling request … for that period". **Do not copy.**
- A failed sample drops the node from the table (`sample_infohashes.go:33-39`), so healthy nodes
  without BEP 51 are evicted rather than marked.

### 2.5 Serving queries (full DHT node)
`internal/protocol/dht/responder/responder.go` handles `ping`, `find_node`, `get_peers`
(token `md5(secret‖ourID‖infohash‖theirID‖theirIP)`, secret never rotates, `:128-137`),
`announce_peer` (stored for serving via `ktable.PutHash`, **not fed to the crawler**,
`:85-98`), and `sample_infohashes` (20 random hashes + 20 nodes, `interval = 10`,
`responder/factory.go:43`; `ktable/query.go:105-116`). It does not set the BEP 43 `ro` flag and
never sends `announce_peer`.

### 2.6 Rate limits

| Limiter | Value | Source |
|---|---|---|
| Outbound KRPC per remote IP | 1 query/s, burst 4, LRU 1000 IPs, TTL 20 s, waits not drops | `internal/protocol/dht/server/factory.go:67` |
| Outbound KRPC global | **none**; bounded only by stage concurrency × 4 s timeout | inferred from `dhtcrawler/factory.go` |
| KRPC query timeout | 4 s | `server/config.go:12-13` |
| Inbound | 50/s overall (burst 20) and 1/s per IP (burst 10); beyond → error 201 | `responder/factory.go:45` |
| BEP 9 TCP connects per peer IP | 2/s, burst 4 | `metainforequester/factory.go:51` |

- UDP socket: `unix.Socket(unix.SOCK_DGRAM, unix.AF_INET, 0)` — domain and type **swapped**,
  works only because both equal 2 on Linux/macOS. IPv4 only; no `AF_INET6`/`udp6` anywhere
  (`server/socket_unix.go:14`).
- Replies matched on transaction ID only, source address not checked (`server/server.go:146-156`).

---

## 3. Metainfo fetch (BEP 9 / BEP 10)

`internal/protocol/metainfo/metainforequester/requester.go` (magnetico-derived, see §0).

- **Transport:** TCP IPv4 only (`DialContext(ctx, "tcp4", …)`, `:130`). No uTP (no uTP library in
  `go.mod`), no IPv6, no MSE/PE.
- **Timeouts:** 3 s dial (`factory.go:36`), 6 s whole request (`metainfo_requester.request_timeout`)
  as a socket deadline, `SetLinger(0)`.
- **Steps:** 68-byte handshake with DHT + LTEP bits; reject on missing LTEP or wrong infohash
  (`:163-212`) → fixed extension handshake `d1:md11:ut_metadatai1eee` (`:231`) → require
  `0 < metadata_size < 10 MiB` and ut_metadata ID 1..254 (`:228-264`) → request every 16 KiB
  piece at once → read; reject/oversize/short non-final piece aborts → SHA-1 vs infohash
  (`internal/protocol/metainfo/parse.go:12-15`). **SHA-1 only**, so pure BEP 52 v2 torrents are
  rejected (v2 handling not traced in depth, UNVERIFIED).
- **Bug:** `if _, writeErr := rw.Write(...); err != nil` (`:231`) tests the wrong variable; a
  failed extension-handshake write is ignored (fails later on read, small impact).
- **Peers:** only the `values` of one `get_peers` to the sampling node. No trackers, no
  iterative lookup, no PEX. 400 fetches in flight at S = 10, peers per hash tried sequentially.

---

## 4. Classifier

- **Content types** (`internal/model/content_type.go:4`): movie, tv_show, music, ebook, comic,
  audiobook, game, software, xxx, plus unknown. The core workflow has no game rule.
- **Video attributes** (`internal/model/video_*.go`): resolution V360p…V4320p; source CAM…BluRay;
  codec H264, x264, x265, XviD, DivX, MPEG2, MPEG4; 3D V3D/V3DSBS/V3DOU; modifiers REGIONAL,
  SCREENER, RAWHD, BRDISK, REMUX. Languages from `languages.csv`, episodes from `episodes_parser.go`.
- **Engine:** YAML workflows with **CEL** conditions (`google/cel-go v0.23.2`); variables
  `torrent`, `result`, `flags.*`, keyword/extension lists (`internal/classifier/cel_env.go:22-31`).
  Actions: if_else, find_match, set_content_type, parse_date, parse_video_content,
  attach_local_content_by_id/by_search, attach_tmdb_content_by_id/by_search, add_tag, delete,
  unmatched, run_workflow (`internal/classifier/action_*.go`). JSON schema provided; user
  extension via `classifier.yml` and `classifier.*`.
- **Default workflow** (`internal/classifier/classifier.core.yml`, 279 lines):
  1. Delete on a **CSAM keyword list** (lines 249-267) — reusable idea for Magnetar's filter.
  2. Type by file-extension byte balance: audiobook, comic, ebook, music, software, xxx.
  3. `parse_date`. 4. Attach by hinted ID (local, then TMDB). 5. `parse_video_content`.
  6. Search by title (local, then TMDB). 7. Optional deletes.
- **Title parsing:** own regexes built with `hedhyw/rex v1.0.0` (`internal/classifier/parsers/video.go`),
  not PTN/guessit. Title match: Levenshtein threshold **5** (`internal/classifier/util.go:9-34`).

---

## 5. TMDB integration

- **Config** (`internal/tmdb/config.go`): `tmdb.enabled` true, `base_url`
  https://api.themoviedb.org/3, `api_key`, `rate_limit` 50 ms (20/s), `rate_limit_burst` 5.
- **Shared default key** hard-coded (`config.go:24`). With it the client warns and drops to
  **1 req/s, burst 8** (`requester_lazy.go:80-88`). An invalid personal key falls back to the
  default key (`:114-125`); a bad default key is a hard error. After a 401 every later request
  fails fast (`requester_fail_fast.go`).
- **HTTP:** resty, 3 retries, 2-20 s waits, 10 s timeout, `semaphore.NewWeighted(2)` →
  **at most 2 in flight for both key types** (correction from fact-check)
  (`requester_lazy.go:90-112`). Key sent as `api_key` query parameter.
- **Endpoints** (`internal/tmdb/client.go`): `/authentication`, `/search/movie`
  (include_adult=true, year), `/movie/{id}`, `/search/tv`, `/tv/{id}?append_to_response=external_ids`,
  `/find/{external_id}`.
- **Cost:** 0 calls if the title is already in local `content`, else 2 (search + details)
  (`internal/classifier/tmdb.go`).
- **Stored** (`internal/tmdb/transformers.go`): title, original title/language, overview, release
  date/year, runtime, popularity, vote average/count, adult (→ xxx); genres and franchise as
  collections; imdb.id, tvdb.id, tmdb.poster_path, tmdb.backdrop_path as attributes.
- **Images not stored:** UI hotlinks `https://image.tmdb.org/t/p/w300/<poster_path>`
  (`webui/src/app/torrents/torrent-content.component.html:2-4`).
- **Not covered:** episodes, music, books, games, anime. Content sources are only tmdb, imdb,
  tvdb (`internal/model/content_source.go`).
- With the default key, enrichment tops out near 3,600 lookups/h, below the claimed crawl rate,
  so a match backlog would likely grow (UNVERIFIED).

---

## 6. Postgres schema
From `internal/model/*.gen.go` and 20 goose migrations.

- **torrents**: PK info_hash (bytea); name, size, private, files_status
  (no_info/single/multi/over_threshold), files_count, generated extension, timestamps.
- **torrent_files**: PK (info_hash, path); index, size, generated extension.
- **torrent_pieces**: piece_length, pieces (only if enabled).
- **torrent_sources**: key, name. **torrents_torrent_sources**: PK (source, info_hash);
  import_id, seeders, leechers, published_at.
- **torrent_hints**: from the importer.
- **torrent_contents**: denormalised search row (content type/source/id, languages, episodes,
  video_*, release_group, tsv, seeders, leechers, published_at, size, files_count).
- **content**: PK (type, source, id) with TMDB fields and tsv; plus content_attributes,
  content_collections (+ content_collections_content), metadata_sources.
- **Other:** torrent_tags, queue_jobs, bloom_filters (large-object oid), key_values.
- **Indexes:** pg_trgm, btree_gin; GIN (content_type, tsv), GIN (content_type, languages),
  GIN tsv, trigram GiST on id, btree on size/seeders/leechers/dates.
- `budgeted_count()` returns the planner estimate when an exact count is too costly.
- Size: FAQ claims ~80 GB per 10 M torrents, ≈ 8 KB/torrent with indexes (UNVERIFIED).

---

## 7. APIs

- **GraphQL** (gqlgen 0.17.64) at `/graphql` with GraphiQL; schema in `graphql/schema/*.graphqls`.
  - Queries: version, workers, health, queue.jobs/metrics, torrent.files/listSources/suggestTags/metrics,
    `torrentContent.search` (queryString, limit/page/offset, totalCount, hasNextPage,
    **infoHashes**, facets, orderBy, cached, aggregationBudget).
  - Mutations: delete, putTags, setTags, deleteTags, reprocess, purgeJobs, enqueueReprocessTorrentsBatch.
- **Torznab** at `GET /torznab/*any`: `t=caps|search|movie|tvsearch|music|book`, `imdbid`/`tmdbid`,
  profiles (`internal/torznab/functions.go`, `parameters.go:7-8`).
- **Other:** `/import`, `/metrics`, `/debug/pprof/*`, `/status`, `/webui`.
- **No authentication**, CORS `*` (`internal/httpserver/config.go:53-61`), pprof on the same port.

### 7.1 Import (`POST /import`)
- NDJSON, optional `X-Import-Id`, streams progress, no auth
  (`internal/importer/httpserver/httpserver.go:55-131`).
- Fields (`internal/importer/importer.go:21-41`): source, infoHash, name, size, private,
  contentType, contentSource, contentId, title, releaseDate, releaseYear, episodes,
  videoResolution, videoSource, videoCodec, video3d, videoModifier, releaseGroup, publishedAt.
- **No file list**; imported rows get `files_status=no_info` (`importer.go:263`), filled later only
  if the crawler happens to sample the hash.
- Buffered 100 rows or 500 ms (`importer/factory.go:30-31`), then classification job
  (`importer.go:213`). RARBG guide: 1.5 M rows in ~10 min (`bitmagnet.io/guides/import.md`, UNVERIFIED).
- **No export endpoint**; backup is pg_dump.
- **The crawler cannot target a given hash** (`faq.md:72-74`): specific torrents must come in via
  `/import`.

### 7.2 Integration paths with Magnetar
1. Magnetar → bitmagnet: NIP-35 event → `{source:"nostr", infoHash, name, size, publishedAt,
   contentSource:"imdb"|"tmdb", contentId}` from `i` tags. bitmagnet classifies and enriches.
2. bitmagnet → Magnetar: GraphQL `torrentContent.search(infoHashes:[…])`, Torznab, or SQL
   `torrent_contents ⨝ content ⨝ content_attributes` (title, year, overview, poster_path, IDs).

---

## 8. Config, ports, resources

- **Config sources:** defaults → `./config.yml` → `$XDG_CONFIG_HOME/bitmagnet/config.yml` → env
  (`internal/config/configfx/module.go:58-67`); env name = dotted path upper-cased with `_`
  (`configresolver/envresolver.go:22`), e.g. `POSTGRES_HOST`, `TMDB_API_KEY`,
  `DHT_CRAWLER_SCALING_FACTOR`.
- **Keys:** `postgres.*` (localhost:5432, user postgres, db bitmagnet); `dht_crawler.*`
  (scaling_factor 10, save_files_threshold 100, save_pieces false, rescrape_threshold 30 d);
  `dht_server.port` **3334/udp**, query_timeout 4 s; `metainfo_requester.request_timeout` 6 s;
  `http_server.local_address` `:3333`; `classifier.*`, `torznab.*`, `log.*`.
- **Ports:** 3333/tcp HTTP, 3334/udp DHT. `docker-compose.yml` also publishes 3334/tcp but nothing
  listens on it (`internal/httpserver/server.go:55` is the only TCP listener).
- **Resources (FAQ, unmeasured, `faq.md:24`):** ~300 MB RAM app, ≥ 1 GB Postgres, ~80 GB per
  10 M torrents, SSD recommended, no upper bound on growth. Compose uses postgres:16-alpine with
  shm 1g and recommends a gluetun VPN (`faq.md:16`, Mullvad/ProtonVPN named).
- **Crawl rate (FAQ, `faq.md:58`):** "anything from 100 to 1,000 torrents per minute"
  (6k-60k/h), slowing as the index grows. First torrents visible within ≤ 10 min (cache TTL,
  `faq.md:28`) plus queue latency. Counter: `bitmagnet_dht_crawler_persisted_total{entity="Torrent"}`.

---

## 9. Licence and reuse

| Component | Licence | Implication for Magnetar (MIT, `~/magnetar/Cargo.toml:9`, no LICENSE file) |
|---|---|---|
| bitmagnet | MIT ("Copyright (c) 2023 bitmagnet contributors") | Ideas, schema, YAML rules reusable; copied/translated code must keep the notice. |
| anacrolix-derived DHT/bencode | MPL-2.0 | File-level copyleft; write from the BEPs (as `bittorrent.md` §0 already plans). |
| `metainforequester/requester.go` | magnetico (AGPL-3.0) text; magnetico credited in docs but its licence not carried | Do not copy or translate. Implement BEP 9/10 from spec. Whether it is legally a derivative is a legal question; only textual similarity was verified. |
| Default TMDB key | TMDB terms | Do not reuse; Magnetar needs its own or a user-supplied key. |

---

## 10. Live measurement

Run 2026-10-25 17:23–17:48 UTC on the laptop (home IP, no VPN, behind NAT; UDP/TCP 3334
published), after the user approved it. Setup: the prepared `compose.yml` (bitmagnet `v0.10.0`
image digest `sha256:cf2c16fa…`, `postgres:16-alpine`), no TMDB key (shared default key,
1 req/s). Samples every 5 min with `sample.sh`; everything removed afterwards (`cleanup.sh`,
verified: no `bm-research-*` containers, volumes, networks or images left).

| t | torrents | files stored (single+multi / over 100-file threshold) | classified | DB | bitmagnet RAM | net in/out (cumulative) |
|---|---|---|---|---|---|---|
| 5 min | 1,655 | 1,562 / 93 | 385 | 21 MB | 147 MB | 118 MB / 19 MB |
| 10 min | 3,421 | 3,227 / 194 | 885 | 34 MB | 204 MB | 249 MB / 64 MB |
| 15 min | 5,267 | 4,964 / 303 | 1,531 | 47 MB | 191 MB | 405 MB / 109 MB |
| 20 min | 7,172 | 6,759 / 413 | 2,188 | 59 MB | 211 MB | 576 MB / 155 MB |
| 25 min | 9,216 | 8,709 / 507 | 2,857 rows (1,301 with no type) | 73 MB | 230 MB | 752 MB / 202 MB |

- **Rate:** steady ~2,000 per 5 min ≈ **370 torrents/min ≈ 22k/h**, no slowdown over 25 min
  (low end of the FAQ's 100–1,000/min). Every stored torrent already has its metainfo, so the
  BEP 9 fetch path keeps up with sampling at this rate.
- **Cost:** bitmagnet 6–15 % of one core, ~230 MB RAM; Postgres ~130 MB; ~8 KB/torrent on disk;
  network ≈ 4 Mbit/s down, 1 Mbit/s up (~2.7 GB/day down).
- **Classifier lags:** after 25 min, 1,857 typed (movie 423, tv_show 409, xxx 310, software 155,
  music 153, ebook 84, audiobook 19, comic 3) + 1,301 unclassified; the rest still queued
  (`queue_jobs` pending grew 15 → 70).
- **Content (40-name random sample + regexes):** mainstream film/TV (e.g. House of the Dragon,
  Mickey 17), FLAC albums, ebooks/PDFs, software; many languages (Cyrillic in 964 names, CJK in
  895). Adult content is a large share (600 names match obvious adult words; coded JAV names
  such as `FC2-PPV-…` are not counted). Some junk folder names ("Сезон 06", system install
  folders).
- These are **stored** torrents, i.e. after a successful metainfo fetch; how many sampled hashes
  bitmagnet discarded is not measured. The Rust prototype in dht-crawling.md resolved only 3–10
  of 40 hashes in 90 s, so a native crawler can still store bitmagnet-like volumes with a low
  per-hash hit rate as long as it samples enough hashes; measure both rates when building it.

## 11. Gaps and bugs (code reading)
1. Reseed interval setting ignored (`factory.go:84` vs `config.go:31`).
2. BEP 51 interval overridden to 60 s (`sample_infohashes.go:62-66`).
3. Node dropped after one failed sample (`sample_infohashes.go:33-39`).
4. Extension-handshake write error ignored (`requester.go:231`).
5. `unix.Socket` arguments swapped (`socket_unix.go:14`).
6. Replies matched on transaction ID only (`server.go:146-156`).
7. IPv4 only; no uTP, BEP 42, announce, `ro` flag.
8. Peers and scrape each come from a single node.
9. No HTTP auth, CORS `*`, pprof exposed.

## UNVERIFIED
- FAQ crawl rate, RAM, disk-per-torrent figures.
- Bloom filter memory (~2.5 MB estimate).
- Metadata fetch success rate; share of uTP-only peers.
- BEP 52 handling; `next`/`dev` branches.
