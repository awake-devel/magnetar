# Configuration reference

The authoritative definition is ARCHITECTURE.md §4 (setting keys), §8.1 (`FfiConfig`) and §9
(server CLI and env). This page collects them in one place.

## Server: `magnetar-server`

```
magnetar-server serve                 # API + /relay endpoint + crawler, liveness, trust, notifier, NIP-66 loops
magnetar-server migrate               # apply sqlx migrations (idempotent)
magnetar-server crawl-once [--seconds N]
magnetar-server selftest              # DB, DNS, TLS (NIP-11 of a seed relay), UDP
magnetar-server enrich [--reimport] [--rematch]   # refresh IMDb datasets, link all pending torrents, JSON report
magnetar-server liveness [--limit N]  # seed-check batches now (configured rates), one JSON line per batch
magnetar-server attrs [--force]       # parse release attributes (language, quality, …) of torrents without current ones, JSON report
magnetar-server --version
```

### Environment

| Variable | Default | Notes |
|---|---|---|
| `DATABASE_URL` | none (required) | TCP `postgres://magnetar:<pw>@127.0.0.1:5432/magnetar` (in `compose.yml` the host is the `db` service), or a unix socket `postgres://magnetar@localhost/magnetar?host=/run/postgresql` |
| `MAGNETAR_DB_POOL` | `10` | sqlx pool size: Postgres connections the server may open (ARCHITECTURE.md D37). Keep it below the server's `max_connections` |
| `MAGNETAR_BIND` | `127.0.0.1:8787` | The image sets `0.0.0.0:8787` (inside the container; the host side is `MAGNETAR_PUBLISH`). Natively, keep loopback and put Tailscale serve or a reverse proxy in front (docs/DEPLOY.md) |
| `MAGNETAR_API_TOKEN` | none (required for `serve`) | `Authorization: Bearer <token>` on everything except `GET /health`. The `/relay` WebSocket also accepts `?token=` |
| `MAGNETAR_ROOT_NPUB` | unset | npub or hex. The root of the web of trust. Can also be set via `PUT /api/v1/settings/trust` |
| `MAGNETAR_NTFY_URL` | unset (e.g. `https://ntfy.sh`) | Root URL. Publishing uses the JSON form (`POST <root>/`) |
| `MAGNETAR_NTFY_TOPIC` | unset (e.g. `magnetar-$(openssl rand -hex 12)`) | Topics are public secrets. ntfy.sh allows 250 messages a day; Magnetar sends at most one per poll (every 15 min, so ≤ 96 a day) covering all saved searches with new hits (D34) |
| `MAGNETAR_DOWNLOADS` | `true` | Start the librqbit download engine with `serve`. `false` saves memory and disk on a device that only indexes; download requests then answer `downloads_unavailable` |
| `MAGNETAR_DOWNLOAD_DIR` | `~/Downloads/magnetar` (image: `/data/downloads`) | librqbit output for server downloads |
| `MAGNETAR_CRAWL_INTERVAL_SECS` | `900` | Pause between crawl passes, at least 60. `0` = continuous: passes run back to back with a 30 s pause (120 s after a failed pass or when every relay is backed off). Use it on an always-on server with bandwidth to spare. Indexer metadata and saved-search notifications still run at most every 15 min (ARCHITECTURE.md D32) |
| `MAGNETAR_CRAWL_MAX_RELAYS` | `30` | Relays crawled per pass (at least 1). Seeds come first, then the best-yielding stored relays |
| `MAGNETAR_CRAWL_EXPLORE_RELAYS` | `10` | Of `MAGNETAR_CRAWL_MAX_RELAYS`, slots kept for relays with no events yet: never-tried first, then the one whose last success is oldest. This is how newly discovered relays (NIP-65 lists, relay hints, kind-3 maps) get crawled. `0` disables exploration |
| `MAGNETAR_SEED_RELAYS` | built-in list | comma-separated `wss://` URLs |
| `MAGNETAR_NIP66` | `true` | NIP-66 relay discovery with `serve`: at startup, then every 6 h, new relays from monitor events are added with source `nip66` (ARCHITECTURE.md D35) |
| `MAGNETAR_NIP66_MONITORS` | `wss://relaypag.es, wss://monitorlizard.nostr1.com, wss://relay.nostr.watch, wss://nos.lol, wss://relay.damus.io` | comma-separated relays queried for kind 10166/30166 monitor events |
| `MAGNETAR_NIP66_TRUSTED` | built-in list of 12 monitor pubkeys (`nip66::DEFAULT_TRUSTED_MONITORS`) | comma-separated hex pubkeys of trusted NIP-66 monitors. They get their own request and page budget, and only relays they report escape the cap of 100 new relays per run from other announced monitors (ARCHITECTURE.md D36) |
| `MAGNETAR_SEARCH_INDEX` | `true` | Embedded Tantivy search index (ARCHITECTURE.md D43, docs/SEARCH.md). Built in the background at the first start (~465 k infohashes: about 35 s on a laptop; searches use Postgres meanwhile), then kept current within ~1–2 s through a change log written by database triggers. `false` = every search runs in Postgres (slow on large indexes) and the change log is emptied hourly; an existing index directory is removed, since it would go stale |
| `MAGNETAR_SEARCH_INDEX_DIR` | `<state dir>/search-index` (`$XDG_STATE_HOME/magnetar/search-index` or `~/.local/state/magnetar/search-index`; image: `/data/state/magnetar/search-index`) | Index files (derived data: delete the directory to force a rebuild). About 0.7 KiB per infohash on disk (~320 MB for 465 k); the server needs exclusive use of it (one `serve` per directory, and one `serve` with the index per database: two would consume each other's change log). A non-empty directory without Tantivy files is never deleted: the index then stays off (searches use Postgres) |
| `MAGNETAR_LIVENESS_BATCH` | `2000` | Seed checks (docs/LIVENESS.md, ARCHITECTURE.md D45): infohashes per batch (1–50 000). Batches run back to back while a full batch is due |
| `MAGNETAR_LIVENESS_INTERVAL_SECS` | `60` | Pause after a batch that emptied the queue (at least 5) |
| `MAGNETAR_LIVENESS_TRACKER_RPS` | `1` | Scrape requests per second per tracker; one request carries 74 infohashes (UDP) or 50 (HTTP). A failing tracker is skipped for 2 min, doubling up to 6 h |
| `MAGNETAR_LIVENESS_TRACKER_CONCURRENCY` | `16` | Trackers scraped at the same time |
| `MAGNETAR_LIVENESS_DHT` | `true` | DHT (BEP 33) for infohashes no tracker answered. `false` leaves them unknown |
| `MAGNETAR_LIVENESS_DHT_CONCURRENCY` | `32` | DHT walks at the same time (~25–50 nodes asked per walk). 32 cleared ~29 k torrents an hour on a home line in the measurement; lower it on a weak connection |
| `RUST_LOG` | `info` | tracing `EnvFilter` syntax, e.g. `info,magnetar_core=debug` |
| `MAGNETAR_IMDB` | `true` | Offline IMDb identification (D40, docs/ENRICHMENT.md): downloads the IMDb non-commercial datasets (~293 MB: title.basics, title.ratings, title.episode) and imports them (~585 MB in Postgres). `false` keeps strong-ID linking (i tags, description links) and skips all downloads |
| `MAGNETAR_IMDB_AKAS` | `false` | Also import `title.akas` (+519 MB download, +~300 MB in Postgres, import ~4.5 min instead of ~1.5): localised titles, +2.8 points correct on French YGG releases |
| `MAGNETAR_DATASET_DIR` | `<state dir>/datasets` (`$XDG_STATE_HOME/magnetar/datasets` or `~/.local/state/magnetar/datasets`) | Where dataset files are kept between refreshes (conditional GET; a `<file>.last-modified` sidecar stores the server's date) |
| `MAGNETAR_IMDB_REFRESH_HOURS` | `168` | Conditional re-download every N hours; the tables are rebuilt and swapped only when a file changed (or the UNLOGGED tables were emptied by a crash) |
| `MAGNETAR_ANIME_DB` | `true` | Offline anime identification with `serve` (docs/ENRICHMENT-ANIME.md, D41): imports anime-offline-database + Fribb anime-lists (checked weekly, conditional GET, ~14 MB download, ~40 k rows) and links anime-category torrents to `media`. `false` = no download, no matching. One-shot: `magnetar-server enrich-anime [--force]` prints a JSON report. Needs the `media`/`torrent_media` tables of the IMDb enrichment |
| `MAGNETAR_ANIME_AOD_URL` | `https://github.com/cedya77/anime-offline-database/releases/latest/download/anime-offline-database.jsonl.zst` | URL or local path of the AOD JSONL (`.zst` or plain). Use `…/releases/download/2026-43/…` to pin a weekly release, or a local file for offline / air-gapped installs |
| `MAGNETAR_ANIME_FRIBB_URL` | `https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-full.json` | URL or local path of Fribb `anime-list-full.json`; pin with a commit SHA instead of `master`. `none` = AOD only: no season remap and no IMDb/TMDB/TVDB ids (Fribb has no licence file, see docs/ENRICHMENT-ANIME.md) |
| `MAGNETAR_TMDB_TOKEN` | unset = TMDB off | Your own TMDB API Read Access Token (v4, sent as `Authorization: Bearer`) or v3 API key (32 hex, sent as `api_key`). Only the server uses it; never put it in the app. Posters/overviews/episode names for matched media, fetched lazily by ID and cached ≤ 6 months (docs/ENRICHMENT-TMDB.md, D42) |
| `MAGNETAR_TMDB_LANGS` | `en-US,fr-FR` | TMDB languages, preferred first; the overview shown is the first language that has one. One details request covers all of them (`append_to_response=translations`); episode names cost one request per language |
| `MAGNETAR_IMAGE_CACHE_DIR` | `$XDG_CACHE_HOME/magnetar/images`, else `~/.cache/magnetar/images` | Content-addressed poster files (`w342`, `<sha256[0..2]>/<sha256>.<ext>`), served by `GET /api/v1/media/{id}/poster`. Files no row references are removed with the 6-month purge |
| `MAGNETAR_TMDB_RPS` | `20` | Token bucket for all TMDB requests (API + images), bursts of up to 5. TMDB's documented ceiling is around 40/s; 429/5xx are retried 3 times honouring `Retry-After` |
| `MAGNETAR_TMDB_PREFETCH_PER_HOUR` | `60` | Background prefetch of media matched in the last 7 days without fresh TMDB data, most releases first (checked every 10 min). `0` = only when a detail is opened |

`GET /api/v1/stats` reports the crawler under `crawler`: `running`, `last_run` (unix end time of
the last pass), `last_report`, `passes` since start, `last_error` and `next_run` (while pausing),
and the search index under `search_index` (null when disabled): `state` (`building` | `ready` |
`error`), `docs`, `build_done`/`build_total`, `build_secs`, `last_commit`, `pending` changes,
`lag_secs` (age of the oldest pending change), `bytes` on disk, `error`, and the seed checks
under `liveness` (null from servers older than D45): `enabled`, `torrents`, `never_checked`,
`due_now`, `checked_24h`, `checks_per_hour`, `alive`/`dead`/`unknown`, `trackers`,
`trackers_backed_off`, `dht_nodes`, `last_batch` (a `BatchReport`) and `last_batch_at`
(docs/LIVENESS.md).

Release attributes (ARCHITECTURE.md D44, docs/FILTERS.md) need no configuration: `serve` parses
new torrents as they arrive and, at startup, any torrent stored before D44 or by an older parser
(`release::PARSER_VERSION`), in chunks of 2000 off the async runtime; `magnetar-server attrs`
does the same once (`--force`: every torrent again). `GET /health` lists `features`
(`filters`, `facet_set`, `media_releases`, `liveness_history`); the app hides what a server
does not list.

librqbit keeps its session and DHT state in `<state dir>/downloads`, where the state dir is
`$XDG_STATE_HOME/magnetar` (default `~/.local/state/magnetar`). The image keeps it under
`/data/state` (docs/DOCKER.md).

### Docker Compose only

`compose.yml` reads `.env` (template: `.env.example`); docs/DOCKER.md lists which of the
variables above it passes to the server. Two more are used by compose itself:

| Variable | Default | Notes |
|---|---|---|
| `MAGNETAR_VERSION` | `latest` | image tag of `ghcr.io/awake-devel/magnetar`: `latest` (newest non-prerelease), `X.Y.Z`, `X.Y.Z-rc1`, or `edge` (newest `main`) |
| `MAGNETAR_PUBLISH` | `127.0.0.1:8787` | where the host publishes the API (exact format in docs/DOCKER.md). Change it only to put it on a LAN or Tailscale address |

The Postgres credentials and volumes are described in docs/DOCKER.md.

### Settings stored in the database (key/value)

| Key | Type | Default |
|---|---|---|
| `trust.root` | hex pubkey | from `MAGNETAR_ROOT_NPUB` |
| `trust.threshold` | int 0-100 | 20 |
| `crawl.seed_relays` | JSON array | built-in list |
| `liveness.enabled` | bool | |
| `ntfy.url`, `ntfy.topic` | string | from env |

### DHT crawler (stages 1–4: discovery, metadata fetch, classification and blocklist, search and ops)

Designed in docs/DHT.md (ARCHITECTURE.md D49–D75, D93). Stage 1 implements discovery: BEP 51
sampling, passive capture, triage into `dht_seen` and peer collection; stage 2 downloads and
verifies each torrent's metadata from its peers (BEP 9, TCP); stage 3 classifies it (category,
adult tag) and passes every name through the hard illegal-content blocklist before anything
is stored (D68, D69). Stored DHT torrents get release attributes, seed checks and IMDb/anime
links. Since stage 4 they are search results mixed equally with Nostr ones (D51, D73: they
pass any trust threshold, carry `origin` `dht` or `both`, and have a Source filter chip), the
crawler shows up in `/api/v1/stats`, `/api/v1/stats/health` and `/health` (feature `dht`), and
`serve` maps its UDP ports with UPnP (D72).

| Variable | Default | Notes |
|---|---|---|
| `MAGNETAR_DHT` | `off` | `off`, `polite` (BEP 51 sampling only, read-only node) or `aggressive` (sampling + passive listening on open UDP ports, UPnP). Crawling shows the host's IP in swarms: opt in deliberately |
| `MAGNETAR_DHT_PORT` | `6891` | first UDP port (aggressive); virtual node *i* uses `PORT + i` |
| `MAGNETAR_DHT_NODES` | `4` aggressive, `1` polite | 1–8 (BEP 42 gives 8 valid node-id prefixes per IPv4 address) |
| `MAGNETAR_DHT_QPS` | `200` / `50` | outbound DHT queries per second, all kinds (router benchmark: 300 safe, 400 breaks the neufbox) |
| `MAGNETAR_DHT_POOL` | `1000000` / `200000` | nodes kept for sampling (1000–20000000) |
| `MAGNETAR_DHT_FETCH_KNOWN` | `false` | also queue infohashes already known from Nostr |
| `MAGNETAR_DHT_PEER_LOOKUPS` | `64` / `8` | peer collections running at once (1–8192; each is a light task, the KRPC rate is the real limit) |
| `MAGNETAR_DHT_LOOKUP_TIMEOUT_MS` | `1500` | soft per-query timeout of peer lookups, ms (100–4000; D93). A node silent this long no longer holds up the lookup: the next node is asked, and its answer is still used if it comes within the 4 s hard timeout. Also applies to the `get_peers` sent to the node that sampled a hash. 4000 = no widening (the old behaviour) |
| `MAGNETAR_DHT_PEERS_PER_HASH` | `12` | peers the metadata fetcher tries per infohash (1–64), 4 at once; a lookup stops once it knows this many (D93) |
| `MAGNETAR_DHT_FETCH_CONCURRENCY` | `128` / `16` | metadata fetches in flight (1–2048); each opens ≤ 4 TCP connections at once, within the two limits below |
| `MAGNETAR_DHT_TCP_RATE` | `150` | new TCP connection attempts per second, all fetches (1–2000). Each attempt is a NAT entry on the router: on the user's SFR neufbox 200/s was safe and 400/s cut the line (docs/research/router-bench.md) |
| `MAGNETAR_DHT_TCP_MAX` | `350` | TCP connections open at once (1–10000); the same router held ~440 |
| `MAGNETAR_DHT_PROBE` | `1.1.1.1:443,9.9.9.9:443` | router health probe: one TCP connect every 5 s, alternating targets; when latency triples (or ≥ 40 ms more) or 3 of 6 fail, DHT queries and TCP attempts are halved for 5 min. `off` disables it. ICMP is not used: blocked or rate-limited on that line |
| `MAGNETAR_DHT_METADATA_MAX` | `16777216` | largest info dictionary accepted, bytes (16384–67108864); larger torrents are marked `oversize` and not retried |
| `MAGNETAR_DHT_UPNP` | `true` in aggressive (`serve`) | map the crawler's UDP ports on the router with UPnP IGD (external = internal port, 1 h lease renewed every 30 min, removed at a clean stop; D57, D72). Polite mode has nothing to map. The bounded `dht` command maps only with an explicit `true`. Only UDP: the crawler never listens on TCP. Builds without the `upnp` cargo feature ignore it |
| `MAGNETAR_DHT_UPNP_GATEWAY` | the default gateway (`/proc/net/route`) | `host[:port]` (port 1900) for a unicast SSDP M-SEARCH, tried before the multicast one (the user's MiniUPnPd answers unicast only); required in Docker |
| `MAGNETAR_DHT_UPNP_LOCAL_IP` | the address the route to the gateway leaves from | LAN address the router forwards to; required in Docker (the host's LAN address) |
| `MAGNETAR_BLOCKLIST_FILE` | unset | extra illegal-content terms, one per line, 1–3 words each (`context:` prefix = only for adult torrents, `#` comments). Adds to the built-in list, for the DHT crawler and Nostr ingest alike; nothing can disable the list. A file that cannot be read or has a malformed line stops the server at start (the error names the line number, never its text) |

Each option also exists as a flag (`--dht aggressive`, `--dht-nodes 2`, …). Bounded run with a
JSON report: `magnetar-server dht --seconds 270 [--mode aggressive] [--dry-run]`. Measured on
the laptop (aggressive, no port opened, 270 s): ~1.6 % of one core, 47 MB RSS; see
docs/DHT.md §13.

### Built-in seed relays (ARCHITECTURE.md §9)

`wss://nostr.oxtr.dev, wss://relay.nostr.net, wss://nos.lol, wss://relay.primal.net,
wss://relay.damus.io, wss://relay.snort.social, wss://nostr.mom, wss://nostr21.com,
wss://nostr.wine, wss://nostr.land, wss://offchain.pub, wss://nostr.bitcoiner.social`.
The indexers used for kinds 0/3/10000/10002 are `wss://purplepag.es`,
`wss://user.kindpag.es` and `wss://indexer.coracle.social`.

## Android app settings

Settings map to `FfiConfig` (ARCHITECTURE.md §8.1):

| Setting | `FfiConfig` field | Default / notes |
|---|---|---|
| Server URL | `server_url` | empty, meaning offline only (local crawl and search). Typically `https://<machine>.<tailnet>.ts.net`. `http://` works too, with a warning unless the host is loopback or Tailscale (ARCHITECTURE.md D39) |
| API token | `api_token` | the server's `MAGNETAR_API_TOKEN` |
| Root npub | `root_pubkey` | npub, nprofile or hex |
| Trust threshold | `trust_threshold` | 20. Results below it are hidden (`min_trust`) |
| Cache cap (MB) | `cache_cap_mb` | `cleanup` evicts only torrents that are synced and not pinned, by LRU |
| Relays | `seed_relays` | the phone crawls at most 8 relays per run |
| Download directory | `download_dir` | app-private `getExternalFilesDir(DIRECTORY_DOWNLOADS)/magnetar` |
| ntfy | | server-side; the app falls back to local notifications |

Background work is a WorkManager job every hour, and only while the phone is **unmetered
and charging**: crawl, sync, cleanup, then poll saved searches.
