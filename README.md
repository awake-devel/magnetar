# Magnetar

Magnetar is a torrent index. It finds torrents in two places:

- **Nostr** ([nostr.com](https://nostr.com)): it crawls relays for NIP-35 torrent events
  (kind 2003) and checks every signature.
- **The BitTorrent DHT**: a native crawler, which normally runs on separate crawl workers.

It stores and indexes the results in Postgres. It also:

- rates each Nostr publisher with a web of trust built from your own follows;
- checks whether swarms are alive;
- links releases to IMDb and TMDB titles;
- optionally reads a few blocks of each file to learn what it really contains (header
  probes, off by default).

You use it through an Android app or a web UI.

It has three parts:

- **Server**: `magnetar-server serve`, with Postgres. It holds the index, the search engine
  and the API, and serves the web UI. It runs anywhere Docker or Linux runs: a Linux PC, a
  VPS, a NAS, a Raspberry Pi, or Docker Desktop on Mac and Windows.
- **Crawl workers** (optional): `magnetar-server worker` on other machines and internet
  lines. They take a share of the seed checks, run their own DHT crawler and, if enabled,
  header probes. They have no database: they talk to the server over its API with a worker
  token (docs/WORKERS.md).
- **Clients**: the Android app and the browser UI. Both are thin clients of the server's REST
  API (D79, D132). They have no crawler, no local search engine and no downloads.

> Status: **v0.3.2 pre-release** (tags `v0.3.2-probes*`). Rows in the feature table say what was
> tested and how. Anything marked **UNVERIFIED** is untested.
> [ARCHITECTURE.md](ARCHITECTURE.md) is the contract (decision rows D1–D145). When it and this
> file disagree, ARCHITECTURE.md wins.

## Architecture

```
       Nostr relays (wss://)                      BitTorrent DHT + trackers
   nos.lol, relay.primal.net, ...                         |
   + NIP-65 lists of publishers            +--------------+---------------+
          |  REQ 2003/2004/1111            |                              |
          v                                v                              v
+-----------------------------------+   +---------------------------+  +---------------------------+
| magnetar-server serve (main)      |   | worker "eee" (own line)   |  | worker "rpi" (own line)   |
|  Nostr crawler (every 15 min)     |   |  DHT crawler (BEP 5/51/9) |  |  DHT crawler, seed checks |
|  seed checks: UDP/HTTP scrape,    |   |  seed checks              |  |  header probes (opt-in)   |
|    DHT BEP 33 fallback            |   |  header probes (opt-in)   |  +-------------+-------------+
|  web of trust, classification,    |   +-------------+-------------+                |
|    blocklist, adult filter        |                 | HTTPS + worker token          |
|  IMDb / TMDB enrichment           |<----------------+-------------------------------+
|  search index, saved searches     |   /api/v1/worker/* (lease, results, uploads)
|    -> ntfy                        |
|  axum API + web UI 127.0.0.1:8787 |<-- HTTPS + Bearer token / session cookie --+
|  /relay WS (NIP-01 + NIP-77)      |   (Tailscale serve or a reverse proxy)     |
+----------------+------------------+                                            |
                 | TCP or unix socket          +---------------------------------+--+
                 v                             | Android app (Kotlin + Compose) and  |
       Postgres 18 (tsvector + pg_trgm)        | web UI (browser, PWA): REST only    |
                                               +-------------------------------------+
```

The main server can run the DHT crawler itself (`MAGNETAR_DHT`), but it is off by default.
On a fragile home router, keep it off and leave the DHT to workers on other lines (read
"Network load" below).

The crates are:

- `crates/core`: shared logic, no database driver. It is also built for the phone without the
  `dht-crawl` feature.
- `crates/server`: Postgres, the API, the web UI, the relay endpoint, the daemons and the
  worker mode.
- `crates/sniff`: the content-based file type and video/audio detector that header probes
  run on the bytes they fetch (crates/sniff/README.md).

`android/` is the app (package `app.magnetar`). Since the rebuild it is a Kotlin client of the
server's REST API (docs/APP.md, D79). The Rust `crates/ffi` library it used before is deleted.

## Features

"Tested" means automated tests plus, for the early rows, an end-to-end run of the app on an
Android 17 (API 37) x86_64 emulator against a live server crawling real relays. Rows marked
**Removed** describe the v0.1.0 app. The rebuilt app (docs/APP.md, D79) dropped them.

| Feature | Where | Status |
|---|---|---|
| Crawl NIP-35 kind 2003 + comments (2004, 1111 `K=2003`) with `until` pagination and per-relay cursors | core, both | Tested (live: 2,675 torrents in 47 s from 2 relays) |
| Signature and id verification on every ingest path | core | Tested |
| Lenient NIP-35 parser (btih alias, `name;size` files, proxy tags, YGG/Nyaa/TPB bridge quirks) | core | Tested on 112 real fixture events |
| Relay discovery (seed list, NIP-65, kind 3 maps, tag hints) | core | Tested |
| Full-text search: Postgres `tsvector` + `pg_trgm` and an in-memory search index with facets (docs/SEARCH.md) | server | Tested. The phone's offline SQLite FTS5 search is **Removed** (D79) |
| Native DHT crawler (docs/DHT.md, D49–D75): KRPC node with BEP 42 ids, BEP 51 sampling, passive capture, BEP 9 metadata fetches, a paced query and TCP budget with a network watchdog, a fail-closed content screen (hard blocklist, adult classifier) | server, worker | Runs on the crawl workers. Off on the main server by default (`MAGNETAR_DHT=off`). Read "Network load" below before turning it on at home |
| Crawl workers (docs/WORKERS.md, D96–D100, D110–D113): seed checks and DHT crawling from other machines over the API with per-worker tokens, leases, hourly caps and tracing | worker, server | Running on two workers (an x86_64 netbook and a Raspberry Pi 3B) since 2026-10-31 |
| IMDb and TMDB enrichment (docs/ENRICHMENT.md, ENRICHMENT-ANIME.md, ENRICHMENT-TMDB.md, D40–D42): titles, years, ratings, episodes, posters and overviews linked to each infohash, never republished. Due infohashes come from a trigger-filled queue (D144) instead of a 2.6 s anti-join per batch | server | Tested. Queue speed-up on production **UNVERIFIED** until measured after deploy |
| Filters (category, size, date, seeders, hide dead, trust) and sorts (relevance, date, size, seeders) | both | Tested (hide dead: no dead torrent was available to observe) |
| Release filters like a private tracker: audio and subtitle language (French scene tags VFF, VFQ, VF2, VFI, VOF, TRUEFRENCH, MULTi, VOSTFR, SUBFRENCH, … and 30 other languages), resolution, source, codec, HDR, audio, genre, IMDb rating, year; live counts next to every choice; sort by rating; saved searches and notifications keep them | core, server, app | New (D44, docs/FILTERS.md): parser checked by hand on 320 production titles (first pass: languages 97.5 %, every field 93.1 %); server and phone cache in automated tests; app built and unit-tested, **UNVERIFIED** on a device or emulator |
| Title page: every release of a movie or series grouped by season/episode, quality and language, with seeders | server, app | New (D44): server and offline cache in automated tests; app screen **UNVERIFIED** on a device or emulator |
| Detail: files, trackers, infohash, npub, open/copy magnet, pin, hide, block publisher | app | Tested |
| Detail "Detected" card (D121): what a header probe read per file (container, video tracks with resolution, HDR and Dolby Vision profile, audio tracks with language, channels, Atmos / DTS:X, subtitle languages, console ids, warnings; track names were not shown until D131, which shows them as VLC does), hidden when a torrent has no probe | app, server | New (D121, docs/APP.md): formatting unit-tested, app built; **UNVERIFIED** on a device or emulator |
| Web-of-trust score 0-100 with explanation (follows, mutes, reports, PoW bonus) | core | Tested |
| Swarm liveness: UDP (BEP 15), HTTP scrape (BEP 48), DHT scrape (BEP 33), TTL cache | core | Tested against real trackers and the DHT |
| Seed checks at scale on the server: every torrent re-checked by age, viewed/searched ones first, per-tracker rate limits and backoff, seeders history, backlog in Stats → Status | server, app | New (D45, docs/LIVENESS.md): ~29 k torrents/h measured on real trackers and the DHT (0.5 M backlog ≈ 17 h, 3 % of a core); Status card built (its ETA helper unit-tested), **UNVERIFIED** on a device or emulator |
| Stats tab: charts of what the index holds (audio languages, resolutions, categories, sources, codecs, HDR, audio, genres, movies/series/anime, years) that cross-filter each other (tap "French": every other chart shows French releases; long-press: search for it), activity over time (torrents, added and published per day, crawled events, relays found, seed checks, database size), a health card; per-torrent seeders/leechers history on the detail page; interactive graphs (pinch, drag, tap a node): web of trust, relays and publishers, a title's releases, a torrent's swarm sources. Offline: counts of the phone's cache | server, app | New (D47, docs/STATS.md): endpoints in automated tests, each under 50 ms with an empty cache on a production copy (501 k torrents); offline counts and fallbacks in ffi tests; app built, layout and number formatting unit-tested, screens **UNVERIFIED** on a device or emulator |
| Phone cache: background crawl (Wi-Fi + charging), sync, LRU/size-cap cleanup | app, ffi | **Removed** (D79). The app keeps only a small cache of recent pages |
| NIP-77 negentropy sync on the server's `/relay` endpoint, batch POST fallback | core, server | Tested (also interop with nostr-sdk). The phone no longer syncs (D79) |
| Saved searches with ntfy push (server) and an hourly poll in the app | server, app | Tested (ntfy against a fake server only) |
| Downloads with librqbit on the server (`MAGNETAR_DOWNLOADS`). The app opens or copies the magnet for another torrent app | server | Tested on x86_64. Phone downloads **Removed** (D79). Keep server downloads off on a fragile router: librqbit's own DHT is unpaced |
| Docker image `ghcr.io/awake-devel/magnetar` (linux/amd64 + linux/arm64) with Postgres 18 in `compose.yml` | Docker, CI | New: see docs/DOCKER.md for what was verified. arm64 is built natively on GitHub's arm64 runners only, **UNVERIFIED** on a device |
| Header probes on crawl workers (off by default): after fetching a torrent's metadata, read the few 16 KiB blocks needed to learn each main file's real type by content, console game ids and video / audio quality (codec, resolution, HDR, audio languages), hash-checked when pieces are small; shown in the API detail and the app's "Detected" card (D121). Downloads file data: read "Legal" below | worker, server | New (D117, docs/PROBES.md): loopback fake peers and Postgres in automated tests; **UNVERIFIED** on real swarms (the eee trial is next) |
| Search filters on what probes read (D122): audio and subtitle track language, picture class (4K and up counts scope films by their width), video codec, HDR (any or a format), audio format incl. Atmos / DTS:X, with live counts in the app's filter sheet; only probed torrents match | server, app | New (D122, docs/FILTERS.md "Probe filters"): index and Postgres fallback in automated tests, timed on 1.8 M synthetic documents; app built and unit-tested, **UNVERIFIED** on a device |
| Track kinds (D123–D126): what each probed subtitle and audio track is for (SDH, closed captions, forced, signs & songs, commentary, audio or text description, honorifics, bilingual, easy reader; original, dub, voice-over, clear dialogue, music only, karaoke, alternate mix), from Matroska / MP4 / TS flags, AC-3 service modes, caption signalling and a multilingual track-name classifier, with no new peer reads; shown per track in the "Detected" card, searchable as "French + SDH" on one track | sniff, worker, server, app | New (D123, docs/PROBES.md §11): hand-built containers and local ffmpeg output in automated tests; **UNVERIFIED**: the non-English and non-French name terms, how often real releases set the Matroska v4 flags or a non-zero bsmod, how often captions are signalled, real swarms; app **UNVERIFIED** on a device |
| Track corrections (D128–D131): in the "Detected" card, each audio and subtitle track is a row numbered as VLC numbers it ("Track 2 · French · AC-3 2.0 · commentary", with its name); an editor corrects its language, kinds and name, marks it as matching VLC or as not in the file, and adds a track the probe missed; corrections are kept apart from the detections, never overwritten by a re-probe, applied to the detail, the language and kind filters and their counts, and can be shown against the detection and undone; the Stats tab's "Header probes" card tells how often each detector source was wrong ("SDH (subtitles) from track names: 4 of 31 wrong") | server, app | New (D128–D131, docs/PROBES.md §12, docs/APP.md): app formatting, request items, view model and API client unit-tested, app built; **UNVERIFIED** on a device; VLC numbering parity per container **UNVERIFIED** |
| Web UI (D132–D135, docs/WEB.md): the server serves a browser client at its own address (`https://<machine>.<tailnet>.ts.net:8443/` behind `tailscale serve`) with the app's screens: search with every filter and live counts, results, torrent detail (media card, swarm with "Check now" and the seed history chart, trust, the "Detected" card with track kinds and track corrections), title pages, saved searches (run, edit, delete, Notify; alerts go out through the server's ntfy, no browser push), Stats (health, breakdowns with cross-filtering, activity, "Where discoveries come from", crawl map, graphs, header probes, "Your corrections") and Settings; a phone-width layout; installable as a PWA with an offline shell and a cache of recently viewed pages; session-cookie sign-in (the browser never stores the API token); strict CSP with Trusted Types | server, web | New: deno unit tests (ports of the app's pure code, checked with the app's test values) and headless-Chromium smoke tests against a local mock server; integration smoke check in Chrome (Claude in Chrome) against the seeded local server of `scripts/webcheck.sh` (sign-in, service worker, Not found, detail, session end); the full parity checklist pass is the next step. **UNVERIFIED**: install and use on Windows and macOS, Safari / iOS, real phones |
| File-kind filter and stats (D137–D140): every probed file is tagged with its content kind (`exe.pe.dotnet`, its group `exe`), searchable by group or exact kind, with a "File kinds" Stats breakdown. Existing probes are tagged by a migration without a re-probe. Every filter group is now one dropdown with live counts, in the web UI and the app | server, web, app | New: server, web and app unit tests, and headless-Chromium checks against intercepted answers; **UNVERIFIED** on a device |
| Probe reach (D141–D143): per-probe diagnostics in the worker log; every candidate peer tried, several at once; partial whole pieces kept; receive-only peer exchange (BEP 11: we read `ut_pex` and never send it) with named per-probe and per-IP bounds | core, worker | New: loopback fake peers in automated tests. The before-numbers come from the eee and rpi workers. **UNVERIFIED**: the gain on real swarms and the extra line cost |
| Postgres write settings in `compose.yml` (D145): `POSTGRES_MAX_WAL_SIZE` (8 GB) and `POSTGRES_SHARED_BUFFERS` (1 GB). Under the defaults, half of the checkpoints were forced and `COMMIT` took up to 25 s on a USB disk | Docker | Applied on production 2026-11-10. **UNVERIFIED** that they remove the stalls |
| Static Linux server binaries (x86_64, aarch64) on every release, with `SHA256SUMS` | CI | See docs/LINUX.md. aarch64: the worker mode runs on a Raspberry Pi 3B (v0.3.1, since 2026-10-31); `serve` on arm64 **UNVERIFIED** |

No analytics, no Google services, no accounts. The app and the web UI talk only to your own
server; the app also talks to ntfy, if you set it up. The server and its workers are the only
parts that contact relays, trackers, the DHT, IMDb and TMDB.

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

- Docker details (variables, volumes, upgrades, backups): [docs/DOCKER.md](docs/DOCKER.md).
  Nightly dumps to another machine: `scripts/backup.sh` (DOCKER.md "Backups")
- Without Docker, with the static binary and your own Postgres: [docs/LINUX.md](docs/LINUX.md)
- Share the crawling with other machines (seed checks and DHT, no database on them): [docs/WORKERS.md](docs/WORKERS.md)
- The DHT crawler on the main server needs the overlay that publishes its UDP ports:
  `docker compose -f compose.yml -f docker/compose.dht.yml up -d`. A plain `up -d` runs
  without it. The overlay defaults to `MAGNETAR_DHT=aggressive`, and aggressive mode turns
  UPnP on unless `MAGNETAR_DHT_UPNP=false`: read "Network load" first.
- Nothing installs systemd units, timers or udev rules by itself. Docker's restart policy
  keeps the containers up; `packaging/` has optional units (a crawl worker, the nightly
  backup) with their install steps, and LINUX.md has examples for starting the binary at boot.

**Why a token?** The server listens on loopback by default and expects a proxy in front.
Every endpoint except `GET /health` requires `Authorization: Bearer <token>`, so whatever
can reach the port (other local users, the tailnet, the LAN) still cannot use the API without
it.

**What the server does not trust.** Everything that arrives from the network is treated as
hostile: torrent metadata from DHT peers is size-checked before it is built, every tracker,
relay and download body has a cap, and no tracker, relay, DHT node or download peer on a
private address (LAN, localhost, tailnet, your own public IP) is ever contacted. A worker token
cannot touch hashes another machine is working on, the torrents a worker adds are capped per
hour and traced to it, and its blocks never hide a torrent published on Nostr (ARCHITECTURE.md D110–D113, docs/WORKERS.md).

### Network load

DHT crawling and peer connections can overwhelm a home router: its NAT table and route cache
fill up long before the line is saturated. On 2026-10-30, a crawl at 75 queries/s with UPnP
took a whole LAN down in about 20 s (D109). On 2026-10-31, 12 queries/s filled the same
router's route cache in 15 min, while the loss-based watchdog stayed quiet. On a consumer
router:

- Leave the main server at `MAGNETAR_DHT=off`. Run the DHT on workers on other lines, or set
  a low `MAGNETAR_DHT_QPS` and watch the router.
- Set `MAGNETAR_DHT_UPNP=false`.
- Set `MAGNETAR_DOWNLOADS=false`: librqbit runs its own DHT, which is not paced.
- Consider `MAGNETAR_LIVENESS_DHT=false`, or keep `MAGNETAR_LIVENESS_DHT_QPS` low.

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
| `MAGNETAR_DHT` | `off` | Native DHT crawler: `off`, `polite` or `aggressive` (docs/DHT.md) |
| `MAGNETAR_DHT_UPNP` | on in `aggressive`, off in `polite` | Map the crawler's UDP ports on the router with UPnP |
| `MAGNETAR_LIVENESS_DHT` | `true` | DHT (BEP 33) fallback for seed checks no tracker answered |
| `MAGNETAR_WEB_UI` | `true` | Serve the browser client (docs/WEB.md) |
| `MAGNETAR_TMDB_TOKEN` | unset | TMDB read token for posters and overviews. IMDb datasets need no key (`MAGNETAR_IMDB`, default `true`) |
| `POSTGRES_MAX_WAL_SIZE`, `POSTGRES_SHARED_BUFFERS` | `8GB`, `1GB` | Docker `db` service only (D145). Keep its `mem_limit` at least twice the buffers |
| `RUST_LOG` | `info` | tracing filter |

Server commands: `magnetar-server serve | migrate | crawl-once [--seconds N] | selftest | --version`.
`serve` applies migrations itself.

### App settings

The app's settings are:

- the server URL and API token, with *Test connection*;
- the trust root, with **Get my key from Amber** (an nsec is refused);
- the trust threshold (default 20, pushed to the server);
- *Show adult content*;
- saved-search notifications.

The app needs the server: it has no local crawler or search engine (docs/APP.md). The web UI
signs in with the API token once and then uses a session cookie (docs/WEB.md).

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
  crawling uses REQ pagination. Negentropy is available on your own server's `/relay` endpoint.
- **Bridge bots dominate** the event volume: Nyaa, TPB and YGG mirrors. They get only as
  much trust as the web of trust gives them. Bridges (events with a `proxy` tag) are exempt
  from the burst penalty, and raising the threshold hides them.
- **Proof of work is not quality.** The bridge bots are the ones that mine PoW. It gives a
  small bonus only (max +25).
- **Private mute lists cannot be read.** They are NIP-44 encrypted to their owner, and
  Magnetar never holds your nsec. Only public mutes count.
- **DHT swarm sizes are estimates** (BEP 33 Bloom filters, saturating around 8000). They are
  not exact seeder counts. They are used only when no tracker answers. "Unknown" is not "dead".
- **A home router is the bottleneck for DHT crawling, not the line.** See "Network load" above
  (D109). About 5 queries/s is the estimate for the untuned router measured.
- **The token is the only lock.** Anyone who has it, or who can read the server's `.env`, has
  full API access. Put HTTPS in front of the server (Tailscale serve or a reverse proxy).
- **The arm64 binary has run on a Pi as a crawl worker only.** The v0.3.1 `aarch64` release
  binary has run `magnetar-server worker` (DHT crawler and seed checks) on a Raspberry Pi 3B
  under the systemd unit since 2026-10-31 with no restart. The full server with Postgres on
  arm64, and the arm64 Docker image, are still **UNVERIFIED** on hardware.

## Legal

Magnetar indexes metadata that other people publish on public Nostr relays and that the DHT
carries (info dicts: names, sizes, hashes). It does not host content. You are responsible for
what you search for, download and share, and for complying with the laws of your country.

**Header probes download parts of files** (crawl workers, `MAGNETAR_PROBE`, off by default;
docs/PROBES.md). With them on, Magnetar no longer handles only metadata: a worker connects to a
torrent's peers and fetches the few blocks it needs to learn what the files really are, and the
legal position becomes blurry.

- In France what is enforced is *sharing*: Arcom's graduated response (formerly HADOPI) acts
  on uploads seen in P2P swarms.
- Downloading a fragment to learn what a torrent contains is not the act that is targeted, but
  it is not explicitly lawful either: a copy from an unlawful source is not covered by the
  private-copy exception (CJEU C-435/12 *ACI Adam*, 2014). Do not read this as "probing is
  legal".
- By default a probe never shares: it sends no piece data, never unchokes a peer, announces
  `HAVE_NONE` (or nothing) instead of real pieces, fetches only the bytes it needs, and writes
  nothing to disk (not dumped, not stored on purpose).
- **Turning `MAGNETAR_PROBE_UPLOAD` on *is* sharing**: the worker then announces and serves
  the whole hash-checked pieces a probe holds to that probe's peers (capped at 4 MiB and
  256 KiB/s per probe).
- Probes and their backfill include adult torrents and BEP 27 private ones (D119): a private
  torrent's peers come from the DHT, against what its private flag asks, and with
  `MAGNETAR_PROBE_UPLOAD` on the worker also shares pieces in that private swarm.
- Other countries differ (Germany, for example, fines downloads too).

This is an **UNVERIFIED legal reading, not legal advice**.

License: MIT (see `Cargo.toml`).
