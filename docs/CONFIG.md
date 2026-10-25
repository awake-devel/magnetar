# Configuration reference

The authoritative definition is ARCHITECTURE.md §4 (setting keys), §8.1 (`FfiConfig`) and §9
(server CLI and env). This page collects them in one place.

## Server: `magnetar-server`

```
magnetar-server serve                 # API + /relay endpoint + crawler, liveness, trust, notifier, NIP-66 loops
magnetar-server migrate               # apply sqlx migrations (idempotent)
magnetar-server crawl-once [--seconds N]
magnetar-server selftest              # DB, DNS, TLS (NIP-11 of a seed relay), UDP
magnetar-server --version
```

### Environment

| Variable | Default | Notes |
|---|---|---|
| `DATABASE_URL` | none (required) | Termux socket form: `postgres:///magnetar?host=/data/data/com.termux/files/usr/tmp&user=magnetar`. Also accepted: `postgres://magnetar@localhost/magnetar?host=<dir>`, or TCP `postgres://magnetar:<pw>@127.0.0.1:5432/magnetar`. sqlx pool of 5 or fewer |
| `MAGNETAR_BIND` | `127.0.0.1:8787` | Never `0.0.0.0` on a phone: every app can reach loopback, and on Wi-Fi other hosts could reach the port too |
| `MAGNETAR_API_TOKEN` | none (required for `serve`) | `Authorization: Bearer <token>` on everything except `GET /health`. The `/relay` WebSocket also accepts `?token=` |
| `MAGNETAR_ROOT_NPUB` | unset | npub or hex. The root of the web of trust. Can also be set via `PUT /api/v1/settings/trust` |
| `MAGNETAR_NTFY_URL` | unset (Termux env: `https://ntfy.sh`) | Root URL. Publishing uses the JSON form (`POST <root>/`) |
| `MAGNETAR_NTFY_TOPIC` | unset (Termux env: random `magnetar-<24 hex>`) | Topics are public secrets. ntfy.sh allows 250 messages a day; Magnetar sends at most one per poll (every 15 min, so ≤ 96 a day) covering all saved searches with new hits (D34) |
| `MAGNETAR_DOWNLOAD_DIR` | Termux env: `~/storage/downloads/magnetar` if `termux-setup-storage` was run, else `~/magnetar/downloads` | librqbit output for server downloads |
| `MAGNETAR_CRAWL_INTERVAL_SECS` | `900` | Pause between crawl passes, at least 60. `0` = continuous: passes run back to back with a 30 s pause (120 s after a failed pass or when every relay is backed off). Use it on an always-on Linux server; keep 900 on a phone. Indexer metadata and saved-search notifications still run at most every 15 min (ARCHITECTURE.md D32) |
| `MAGNETAR_CRAWL_MAX_RELAYS` | `30` | Relays crawled per pass (at least 1). Seeds come first, then the best-yielding stored relays |
| `MAGNETAR_CRAWL_EXPLORE_RELAYS` | `10` | Of `MAGNETAR_CRAWL_MAX_RELAYS`, slots kept for relays with no events yet: never-tried first, then the one whose last success is oldest. This is how newly discovered relays (NIP-65 lists, relay hints, kind-3 maps) get crawled. `0` disables exploration |
| `MAGNETAR_SEED_RELAYS` | built-in list | comma-separated `wss://` URLs |
| `MAGNETAR_NIP66` | `true` | NIP-66 relay discovery with `serve`: at startup, then every 6 h, new relays from monitor events are added with source `nip66` (ARCHITECTURE.md D35) |
| `MAGNETAR_NIP66_MONITORS` | `wss://relaypag.es, wss://monitorlizard.nostr1.com, wss://relay.nostr.watch, wss://nos.lol, wss://relay.damus.io` | comma-separated relays queried for kind 10166/30166 monitor events |
| `MAGNETAR_NIP66_TRUSTED` | built-in list of 12 monitor pubkeys (`nip66::DEFAULT_TRUSTED_MONITORS`) | comma-separated hex pubkeys of trusted NIP-66 monitors. They get their own request and page budget, and only relays they report escape the cap of 100 new relays per run from other announced monitors (ARCHITECTURE.md D36) |
| `RUST_LOG` | `info` (Termux env) | tracing `EnvFilter` syntax, e.g. `info,magnetar_core=debug` |
| `MAGNETAR_ANIME_DB` | `true` | Offline anime identification with `serve` (docs/ENRICHMENT-ANIME.md, D41): imports anime-offline-database + Fribb anime-lists (checked weekly, conditional GET, ~14 MB download, ~40 k rows) and links anime-category torrents to `media`. `false` = no download, no matching. One-shot: `magnetar-server enrich-anime [--force]` prints a JSON report. Needs the `media`/`torrent_media` tables of the IMDb enrichment |
| `MAGNETAR_ANIME_AOD_URL` | `https://github.com/cedya77/anime-offline-database/releases/latest/download/anime-offline-database.jsonl.zst` | URL or local path of the AOD JSONL (`.zst` or plain). Use `…/releases/download/2026-43/…` to pin a weekly release, or a local file for offline / air-gapped installs |
| `MAGNETAR_ANIME_FRIBB_URL` | `https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-full.json` | URL or local path of Fribb `anime-list-full.json`; pin with a commit SHA instead of `master`. `none` = AOD only: no season remap and no IMDb/TMDB/TVDB ids (Fribb has no licence file, see docs/ENRICHMENT-ANIME.md) |

`GET /api/v1/stats` reports the crawler under `crawler`: `running`, `last_run` (unix end time of
the last pass), `last_report`, `passes` since start, `last_error` and `next_run` (while pausing).

The Termux setup also writes these:

| Variable | Used by |
|---|---|
| `MAGNETAR_DB_PASSWORD` | the setup script, as the scram password of role `magnetar` for TCP on 127.0.0.1:5432. The server uses the socket and does not need it |
| `TMPDIR` | `$PREFIX/tmp`. Rust's `temp_dir()` would otherwise fall back to the unwritable `/data/local/tmp` on Android |

The env file is sourced by `sh` with `set -a`, so keep the values single-quoted
(`DATABASE_URL` contains `&`).

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

## Termux Postgres

| Setting | Value | Why |
|---|---|---|
| data dir | `$PREFIX/var/lib/postgresql` | the only path the stock runit service uses |
| socket | `$PREFIX/tmp/.s.PGSQL.5432` | Termux's compiled default, app-private |
| `listen_addresses` | `127.0.0.1` | |
| `max_connections` | 20 | process budget |
| `io_workers` | 1 | PG 18 defaults to 3 worker processes |
| `shared_buffers` | 64MB | |
| `pg_hba` | `local all all trust`, `host all all 127.0.0.1/32 scram-sha-256` | loopback is shared with all apps |

## Android app settings

Settings map to `FfiConfig` (ARCHITECTURE.md §8.1):

| Setting | `FfiConfig` field | Default / notes |
|---|---|---|
| Server URL | `server_url` | `http://127.0.0.1:8787`. Empty means offline-only (local crawl and search) |
| API token | `api_token` | the value of `magnetar-ctl token` |
| Root npub | `root_pubkey` | npub, nprofile or hex |
| Trust threshold | `trust_threshold` | 20. Results below it are hidden (`min_trust`) |
| Cache cap (MB) | `cache_cap_mb` | `cleanup` evicts only torrents that are synced and not pinned, by LRU |
| Relays | `seed_relays` | the phone crawls at most 8 relays per run |
| Download directory | `download_dir` | app-private `getExternalFilesDir(DIRECTORY_DOWNLOADS)/magnetar` |
| ntfy | | server-side; the app falls back to local notifications |

Background work is a WorkManager job every hour, and only while the phone is **unmetered
and charging**: crawl, sync, cleanup, then poll saved searches.
