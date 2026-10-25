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
| `RUST_LOG` | `info` | tracing `EnvFilter` syntax, e.g. `info,magnetar_core=debug` |
| `MAGNETAR_IMDB` | `true` | Offline IMDb identification (D40, docs/ENRICHMENT.md): downloads the IMDb non-commercial datasets (~293 MB: title.basics, title.ratings, title.episode) and imports them (~585 MB in Postgres). `false` keeps strong-ID linking (i tags, description links) and skips all downloads |
| `MAGNETAR_IMDB_AKAS` | `false` | Also import `title.akas` (+519 MB download, +~300 MB in Postgres, import ~4.5 min instead of ~1.5): localised titles, +2.8 points correct on French YGG releases |
| `MAGNETAR_DATASET_DIR` | `<state dir>/datasets` (`$XDG_STATE_HOME/magnetar/datasets` or `~/.local/state/magnetar/datasets`) | Where dataset files are kept between refreshes (conditional GET; a `<file>.last-modified` sidecar stores the server's date) |
| `MAGNETAR_IMDB_REFRESH_HOURS` | `168` | Conditional re-download every N hours; the tables are rebuilt and swapped only when a file changed (or the UNLOGGED tables were emptied by a crash) |

`GET /api/v1/stats` reports the crawler under `crawler`: `running`, `last_run` (unix end time of
the last pass), `last_report`, `passes` since start, `last_error` and `next_run` (while pausing).

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
