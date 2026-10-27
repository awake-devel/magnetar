# Magnetar

Magnetar finds torrents that people publish on [Nostr](https://nostr.com). It crawls relays
for NIP-35 torrent events (kind 2003), checks every signature, and stores and indexes the
results. It rates each publisher with a web of trust built from your own follows, checks
whether swarms are alive, and gives you search, details and downloads in an Android app.

It runs in two places, and both of them crawl:

- **Server**: `magnetar-server`, which uses Postgres. It runs anywhere Docker or Linux runs:
  a Linux PC, a VPS, a Synology NAS, a Raspberry Pi, or Docker Desktop on Mac and Windows.
- **Android app**: keeps a local SQLite cache, so search still works offline. Whatever the
  phone finds is synced up to the server.

> Status: **v0.1.0 pre-release**, the first test build. Everything in the feature table ran
> in automated tests and on an emulator; it has not yet run on a real phone. Anything marked
> **UNVERIFIED** is untested. [ARCHITECTURE.md](ARCHITECTURE.md) is the contract; when it and
> this file disagree, ARCHITECTURE.md wins.

## Architecture

```
                         Nostr relays (wss://)
        nos.lol, relay.primal.net, nostr.oxtr.dev, ... + NIP-65 lists of publishers
               |  REQ kinds 2003/2004/1111 (+ 0/3/10000/10002 from indexers)
       +-------+--------------------------------------+
       |                                              |
       v                                              v
+--------------------------------+      +------------------------------------+
| magnetar-server (Docker/Linux) |      | Android app (Kotlin + Compose)     |
|  crawler loop (every 15 min)   |      |  search, filters, detail, titles,  |
|  liveness: UDP/HTTP scrape,    |      |  saved searches (hourly poll),     |
|    DHT BEP 33 fallback         |      |  Stats + graphs, Settings          |
|  trust: depth-2 web of trust   |      |  OkHttp + kotlinx.serialization;   |
|  saved searches -> ntfy        |      |  no Rust, no downloads (D79)       |
|  librqbit downloads            |      +-----------------+------------------+
|  axum API 127.0.0.1:8787       |<-- HTTPS + Bearer token: REST API only ---+
|  /relay WS (NIP-01 + NIP-77)   |   (Tailscale serve or a reverse proxy)
+---------------+----------------+
               | TCP or unix socket
               v
     Postgres 18 (tsvector + pg_trgm)
```

The crates are `crates/core` (shared logic, no database driver) and `crates/server`
(Postgres, the API, the relay endpoint and the daemons); `android/` is the app (package
`app.magnetar`), a Kotlin client of the server's REST API since the rebuild (docs/APP.md,
D79; the Rust `crates/ffi` library it used before is deleted).

## Features

Status for v0.1.0 (first pre-release). The app rows describe the v0.1.0 app; the rebuilt app
(docs/APP.md) drops the phone cache, phone crawler and downloads. "Tested" means automated tests plus an end-to-end run
of the app on an Android 17 (API 37) x86_64 emulator against a live server crawling real relays.

| Feature | Where | Status |
|---|---|---|
| Crawl NIP-35 kind 2003 + comments (2004, 1111 `K=2003`) with `until` pagination and per-relay cursors | core, both | Tested (live: 2,675 torrents in 47 s from 2 relays) |
| Signature and id verification on every ingest path | core | Tested |
| Lenient NIP-35 parser (btih alias, `name;size` files, proxy tags, YGG/Nyaa/TPB bridge quirks) | core | Tested on 112 real fixture events |
| Relay discovery (seed list, NIP-65, kind 3 maps, tag hints) | core | Tested |
| Full-text search: Postgres `tsvector` + `pg_trgm`, SQLite FTS5 offline on the phone | server, ffi | Tested |
| Filters (category, size, date, seeders, hide dead, trust) and sorts (relevance, date, size, seeders) | both | Tested (hide dead: no dead torrent was available to observe) |
| Release filters like a private tracker: audio and subtitle language (French scene tags VFF, VFQ, VF2, VFI, VOF, TRUEFRENCH, MULTi, VOSTFR, SUBFRENCH, … and 30 other languages), resolution, source, codec, HDR, audio, genre, IMDb rating, year; live counts next to every choice; sort by rating; saved searches and notifications keep them | core, server, app | New (D44, docs/FILTERS.md): parser checked by hand on 320 production titles (first pass: languages 97.5 %, every field 93.1 %); server and phone cache in automated tests; app built and unit-tested, **UNVERIFIED** on a device or emulator |
| Title page: every release of a movie or series grouped by season/episode, quality and language, with seeders | server, app | New (D44): server and offline cache in automated tests; app screen **UNVERIFIED** on a device or emulator |
| Detail: files, trackers, infohash, npub, open/copy magnet, pin, hide, block publisher | app | Tested |
| Web-of-trust score 0-100 with explanation (follows, mutes, reports, PoW bonus) | core | Tested |
| Swarm liveness: UDP (BEP 15), HTTP scrape (BEP 48), DHT scrape (BEP 33), TTL cache | core | Tested against real trackers and the DHT |
| Seed checks at scale on the server: every torrent re-checked by age, viewed/searched ones first, per-tracker rate limits and backoff, seeders history, backlog in Stats → Status | server, app | New (D45, docs/LIVENESS.md): ~29 k torrents/h measured on real trackers and the DHT (0.5 M backlog ≈ 17 h, 3 % of a core); Status card built (its ETA helper unit-tested), **UNVERIFIED** on a device or emulator |
| Stats tab: charts of what the index holds (audio languages, resolutions, categories, sources, codecs, HDR, audio, genres, movies/series/anime, years) that cross-filter each other (tap "French": every other chart shows French releases; long-press: search for it), activity over time (torrents, added and published per day, crawled events, relays found, seed checks, database size), a health card; per-torrent seeders/leechers history on the detail page; interactive graphs (pinch, drag, tap a node): web of trust, relays and publishers, a title's releases, a torrent's swarm sources. Offline: counts of the phone's cache | server, app | New (D47, docs/STATS.md): endpoints in automated tests, each under 50 ms with an empty cache on a production copy (501 k torrents); offline counts and fallbacks in ffi tests; app built, layout and number formatting unit-tested, screens **UNVERIFIED** on a device or emulator |
| Phone cache: background crawl (Wi-Fi + charging), sync, LRU/size-cap cleanup that never deletes unsynced rows | app, ffi | Tested |
| Phone to server sync: NIP-77 negentropy, batch POST fallback | core, server | Tested (also interop with nostr-sdk) |
| Saved searches with ntfy push (server) and local notifications (phone) | server, app | Tested (local notifications; ntfy against a fake server only) |
| Downloads with librqbit on the server and on the phone (fallback: open the magnet in another app) | server, ffi | Tested on x86_64; **arm64 phone downloads UNVERIFIED** |
| Docker image `ghcr.io/awake-devel/magnetar` (linux/amd64 + linux/arm64) with Postgres 18 in `compose.yml` | Docker, CI | New: see docs/DOCKER.md for what was verified. arm64 is built natively on GitHub's arm64 runners only, **UNVERIFIED** on a device |
| Static Linux server binaries (x86_64, aarch64) on every release, with `SHA256SUMS` | CI | New: see docs/LINUX.md. aarch64 built on GitHub's arm64 runners only, **UNVERIFIED** on a device |

No analytics, no Google services, no accounts. The app talks to relays, trackers, your own
server and, if you set it up, ntfy.

## Install

[docs/DEPLOY.md](docs/DEPLOY.md) explains which setup suits which machine (Linux PC, Synology
NAS, Raspberry Pi, VPS, Mac or Windows) and how the phone reaches the server. The short
version, with Docker Compose:

```sh
git clone https://github.com/awake-devel/magnetar && cd magnetar
cp .env.example .env && $EDITOR .env      # set MAGNETAR_API_TOKEN (openssl rand -hex 32) and the DB password
docker compose pull && docker compose up -d
tailscale serve --bg 127.0.0.1:8787       # optional: https://<machine>.<tailnet>.ts.net for the phone
```

Then install `magnetar-<tag>.apk` from the release on the phone, and in Settings enter the
server URL (`https://<machine>.<tailnet>.ts.net`) and the API token.

- Docker details (variables, volumes, upgrades, backups): [docs/DOCKER.md](docs/DOCKER.md)
- Without Docker, with the static binary and your own Postgres: [docs/LINUX.md](docs/LINUX.md)
- Nothing here installs systemd units, timers or udev rules. Docker's restart policy keeps
  the containers up; LINUX.md has examples you can adapt if you want the binary started at
  boot.

**Why a token?** The server listens on loopback by default and expects a proxy in front.
Every endpoint except `GET /health` requires `Authorization: Bearer <token>`, so whatever
can reach the port (other local users, the tailnet, the LAN) still cannot use the API without
it.

## Configuration

See [docs/CONFIG.md](docs/CONFIG.md) for all the details.

### Server environment

The most used variables. Docker reads them from `.env`; the native binary from its env file.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | required | Postgres connection, e.g. `postgres://magnetar:<pw>@db:5432/magnetar` (set by `compose.yml`) |
| `MAGNETAR_BIND` | `127.0.0.1:8787` (`0.0.0.0:8787` inside the image) | API + `/relay` listen address |
| `MAGNETAR_API_TOKEN` | required for `serve` | Bearer token for every endpoint except `/health` |
| `MAGNETAR_ROOT_NPUB` | unset | Root of the web of trust (npub or hex) |
| `MAGNETAR_NTFY_URL`, `MAGNETAR_NTFY_TOPIC` | unset | ntfy server and topic for saved-search notifications. Treat the topic as a secret |
| `MAGNETAR_DOWNLOADS` | `true` | Start the librqbit download engine. `false` on a small device that only indexes |
| `MAGNETAR_DOWNLOAD_DIR` | `~/Downloads/magnetar` (`/data/downloads` in the image) | Server-side download output |
| `MAGNETAR_CRAWL_INTERVAL_SECS` | `900` | Pause between crawler passes. `0` crawls continuously (30 s between passes) |
| `MAGNETAR_CRAWL_MAX_RELAYS` | `30` | Relays crawled per pass |
| `MAGNETAR_CRAWL_EXPLORE_RELAYS` | `10` | Of those, slots for newly discovered relays that have not yielded events yet |
| `MAGNETAR_SEED_RELAYS` | built-in list (ARCHITECTURE.md §9) | Comma-separated relay URLs |
| `MAGNETAR_NIP66` | `true` | NIP-66 relay discovery every 6 h |
| `MAGNETAR_DB_POOL` | `10` | Postgres connections the server may open |
| `RUST_LOG` | `info` | tracing filter |

Server commands: `magnetar-server serve | migrate | crawl-once [--seconds N] | selftest | --version`.
`serve` applies migrations itself.

### App settings

Server URL, API token, root npub, trust threshold (default 20), cache cap (MB), seed relays,
ntfy, and download directory. The app works without a server: it then crawls and searches
locally only.

## Building from source (laptop)

```sh
docker build -t magnetar .                 # the server image, for this machine's architecture
scripts/build-linux.sh x86_64              # dist/magnetar-server-x86_64-linux (static); aarch64 on an arm64 host
cargo build --release -p magnetar-server   # plain native build

scripts/build-android.sh                   # Android: unit tests + debug and release APKs (Gradle)
```

The Android build needs JDK 17+ (27 works with Gradle 9.8.2) and the Android SDK with
compileSdk 37; no Rust (D79). Do not install aws-lc or OpenSSL for the server: TLS is rustls
with ring and webpki-roots. More in [docs/SETUP.md](docs/SETUP.md).

Releases: pushing a tag `vX.Y.Z` makes CI build the image (both architectures) and the Linux
binaries and attach them to a draft release. `scripts/release.sh` then builds and signs the
APK locally, uploads it and publishes the release (dry run by default, `--publish` to act).

## Testing

```sh
scripts/test.sh                  # fmt --check, clippy -D warnings, all tests with Postgres 18, Android build
scripts/test.sh --ignored        # + network tests against real relays and trackers
```

`scripts/test.sh` starts (or creates) the docker container `magnetar-pg` (`postgres:18` on
`127.0.0.1:54329`) and exports `MAGNETAR_TEST_DATABASE_URL`. Every test uses its own schema.
GitHub Actions (`.github/workflows/ci.yml`) runs fmt, clippy and the tests against a
`postgres:18` service on every push and pull request.

## Limitations

These are known from research on live relays and devices. [docs/LIMITATIONS.md](docs/LIMITATIONS.md)
has the full list with sources.

- **relay.damus.io pagination is unreliable.** `COUNT` reports thousands of events, but
  `until` pagination returned 23 and then HTTP 503. An empty page from damus does not prove
  that its history is exhausted.
- **relay.nostr.band is dead** (handshake timeout), so it is not used.
- **NIP-77 negentropy is disabled on most big relays** (damus, nos.lol, primal, nostr.mom), so
  crawling uses REQ pagination. Negentropy is used only between the phone and your own server.
- **Bridge bots dominate** the event volume: Nyaa, TPB and YGG mirrors. They get only as
  much trust as the web of trust gives them. Bridges (events with a `proxy` tag) are exempt
  from the burst penalty, and raising the threshold hides them.
- **Proof of work is not quality.** The bridge bots are the ones that mine PoW. It gives a
  small bonus only (max +25).
- **Private mute lists cannot be read.** They are NIP-44 encrypted to their owner, and
  Magnetar never holds your nsec. Only public mutes count.
- **DHT swarm sizes are estimates** (BEP 33 Bloom filters, saturating around 8000). They are
  not exact seeder counts. They are used only when no tracker answers. "Unknown" is not "dead".
- **Android 15+ caps `dataSync` foreground services at 6 hours per 24 hours.** Phone downloads
  pause and notify at the cap. Use server downloads for long ones.
- **librqbit's own HTTPS tracker requests may fail on Android.** Its platform TLS verifier
  needs JNI. UDP trackers and DHT still work.
- **The token is the only lock.** Anyone who has it, or who can read the server's `.env`, has
  full API access. Put HTTPS in front of the server (Tailscale serve or a reverse proxy).
- **arm64 server builds are UNVERIFIED on hardware.** The image and binary for arm64 are built
  on GitHub's native arm64 runners; they have not run on a Raspberry Pi or an ARM NAS yet.

## Legal

Magnetar indexes metadata that other people publish on public Nostr relays. It does not host,
seed or distribute any content by itself. You are responsible for what you search for,
download and share, and for complying with the laws of your country.

License: MIT (see `Cargo.toml`).
