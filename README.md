# Magnetar

Magnetar finds torrents that people publish on [Nostr](https://nostr.com). It crawls relays
for NIP-35 torrent events (kind 2003), checks every signature, and stores and indexes the
results. It rates each publisher with a web of trust built from your own follows, checks
whether swarms are alive, and gives you search, details and downloads in an Android app.

It runs in two places, and both of them crawl:

- **Server**: `magnetar-server`, which uses Postgres. It can run **on the phone itself
  inside Termux** or on any Linux machine.
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
| magnetar-server (Termux/Linux) |      | Android app (Kotlin + Compose)     |
|  crawler loop (every 15 min)   |      |  WorkManager: crawl -> sync ->     |
|  liveness: UDP/HTTP scrape,    |      |    cleanup -> saved-search poll    |
|    DHT BEP 33 fallback         |      |  magnetar-ffi (Rust, uniffi):      |
|  trust: depth-2 web of trust   |      |    SQLite + FTS5 cache, crawler,   |
|  saved searches -> ntfy        |      |    API client, librqbit downloads  |
|  librqbit downloads            |      +-----------------+------------------+
|  axum API 127.0.0.1:8787       |<-- HTTP + Bearer token -+ search / detail /
|  /relay WS (NIP-01 + NIP-77)   |<-- negentropy sync up --+ downloads / settings
+---------------+----------------+       (batch POST fallback)
               | unix socket
               v
     Postgres 18 (tsvector + pg_trgm)
```

The crates are `crates/core` (shared logic, no database driver), `crates/server`
(Postgres, the API, the relay endpoint and the daemons), `crates/ffi` (SQLite store and
uniffi API for Android) and `android/` (the app, package `app.magnetar`).

## Features

Status for v0.1.0 (first pre-release). "Tested" means automated tests plus an end-to-end run
of the app on an Android 17 (API 37) x86_64 emulator against a live server crawling real relays.

| Feature | Where | Status |
|---|---|---|
| Crawl NIP-35 kind 2003 + comments (2004, 1111 `K=2003`) with `until` pagination and per-relay cursors | core, both | Tested (live: 2,675 torrents in 47 s from 2 relays) |
| Signature and id verification on every ingest path | core | Tested |
| Lenient NIP-35 parser (btih alias, `name;size` files, proxy tags, YGG/Nyaa/TPB bridge quirks) | core | Tested on 112 real fixture events |
| Relay discovery (seed list, NIP-65, kind 3 maps, tag hints) | core | Tested |
| Full-text search: Postgres `tsvector` + `pg_trgm`, SQLite FTS5 offline on the phone | server, ffi | Tested |
| Filters (category, size, date, seeders, hide dead, trust) and sorts (relevance, date, size, seeders) | both | Tested (hide dead: no dead torrent was available to observe) |
| Detail: files, trackers, infohash, npub, open/copy magnet, pin, hide, block publisher | app | Tested |
| Web-of-trust score 0-100 with explanation (follows, mutes, reports, PoW bonus) | core | Tested |
| Swarm liveness: UDP (BEP 15), HTTP scrape (BEP 48), DHT scrape (BEP 33), TTL cache | core | Tested against real trackers and the DHT |
| Phone cache: background crawl (Wi-Fi + charging), sync, LRU/size-cap cleanup that never deletes unsynced rows | app, ffi | Tested |
| Phone to server sync: NIP-77 negentropy, batch POST fallback | core, server | Tested (also interop with nostr-sdk) |
| Saved searches with ntfy push (server) and local notifications (phone) | server, app | Tested (local notifications; ntfy against a fake server only) |
| Downloads with librqbit on the server and on the phone (fallback: open the magnet in another app) | server, ffi | Tested on x86_64; **arm64 phone downloads UNVERIFIED** |
| Termux server: one-command setup, runit services, boot start | scripts | Tested in termux-docker with the real binary (64/64 checks); not yet on a real phone |
| Cross-compiled server binary for Termux (API 24, aarch64 + x86_64) | scripts | Built; needs only bionic libc/libm/libdl |

No analytics, no Google services, no accounts. The app talks to relays, trackers, your own
server and, if you set it up, ntfy.

## Quick start: everything on one phone

This setup runs the server in Termux on the same phone as the app, so it needs no other
machine. It takes about 10 minutes, most of which is package downloads.

1. **Install Termux and Termux:Boot from F-Droid.** Both apps must come from the same source,
   because Termux:Boot has to share Termux's signature. The Play Store build of Termux is a
   different app. Open **Termux:Boot** once so that Android lets it run at boot.
2. **Get the setup script** in Termux:
   ```sh
   pkg install -y curl jq
   curl -fsSLO https://github.com/awake-devel/magnetar/releases/download/v0.1.0/termux-setup.sh
   MAGNETAR_VERSION=v0.1.0 bash termux-setup.sh
   ```
   The script installs Postgres 18 and creates the database. It downloads the server binary
   that matches `uname -m` and checks it against `SHA256SUMS`. It then runs the migrations,
   installs the runit services and the boot script, and waits for `/health`. You can run it
   again at any time, and it never regenerates secrets. Prereleases are not "latest":
   install one with `MAGNETAR_VERSION=v0.1.0-rc1 bash termux-setup.sh`.
3. **Copy the API token** printed at the end. To print it again later, run `magnetar-ctl token`.
4. **Disable the phantom process killer** (Android 12 and later). Without this, Android kills
   Postgres and the server sooner or later.
   - Android 14+: Settings > System > Developer options > **Disable child process restrictions**.
     Leave Developer options switched on, or the setting reverts.
   - Android 12L/13: from a PC, `adb shell settings put global settings_enable_monitor_phantom_procs false`.
   - Also exempt Termux from battery optimisation.
5. **Install the APK** (`magnetar-<tag>.apk` from the same release). You need to allow
   "install unknown apps" for your browser or file manager.
6. **Open the app, go to Settings** and set:
   - Server URL: `http://127.0.0.1:8787`
   - API token: the value from step 3. Long-press in Termux to copy it.
   - Root npub (optional): your own npub, so the web of trust starts from your follows.

   The Status screen should then show the server as reachable.

Manage the server with `magnetar-ctl start|stop|restart|status|logs|token|update`.
[docs/TERMUX.md](docs/TERMUX.md) covers the details, flags (`--no-services`,
`--uninstall-services`, `MAGNETAR_BINARY=`, `MAGNETAR_BUILD=local`) and troubleshooting.

**Why a token on localhost?** Every app on an Android phone can reach `127.0.0.1`. The server
binds to loopback only and requires `Authorization: Bearer <token>` on everything except
`GET /health`. Postgres accepts TCP only with a password and the server itself uses the
private unix socket.

## Configuration

See [docs/CONFIG.md](docs/CONFIG.md) for all the details.

### Server environment (`~/.config/magnetar/env` in Termux)

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | Termux: `postgres:///magnetar?host=$PREFIX/tmp&user=magnetar` | Postgres connection (socket form in Termux) |
| `MAGNETAR_BIND` | `127.0.0.1:8787` | API + `/relay` listen address. Keep it on loopback or a Tailscale IP |
| `MAGNETAR_API_TOKEN` | required for `serve` | Bearer token for every endpoint except `/health` |
| `MAGNETAR_ROOT_NPUB` | unset | Root of the web of trust (npub or hex) |
| `MAGNETAR_NTFY_URL` | `https://ntfy.sh` in the generated env | ntfy server for saved-search notifications |
| `MAGNETAR_NTFY_TOPIC` | random `magnetar-…` | ntfy topic. Treat it as a secret |
| `MAGNETAR_DOWNLOAD_DIR` | `~/storage/downloads/magnetar` (or `~/magnetar/downloads`) | Server-side download output |
| `MAGNETAR_CRAWL_INTERVAL_SECS` | `900` | Pause between crawler passes. `0` crawls continuously (30 s between passes), for an always-on Linux server |
| `MAGNETAR_CRAWL_MAX_RELAYS` | `30` | Relays crawled per pass |
| `MAGNETAR_CRAWL_EXPLORE_RELAYS` | `10` | Of those, slots for newly discovered relays that have not yielded events yet |
| `MAGNETAR_SEED_RELAYS` | built-in list (ARCHITECTURE.md §9) | Comma-separated relay URLs |
| `RUST_LOG` | `info` | tracing filter |

The setup script also writes `MAGNETAR_DB_PASSWORD`, which is the `magnetar` role's password
for TCP connections to 127.0.0.1:5432, and `TMPDIR`. Server commands:
`magnetar-server serve | migrate | crawl-once [--seconds N] | selftest | --version`.

### App settings

Server URL, API token, root npub, trust threshold (default 20), cache cap (MB), seed relays,
ntfy, and download directory. The app works without a server: it then crawls and searches
locally only.

## Using a real server over Tailscale (later)

The same server runs on a Linux box (see [docs/SETUP.md](docs/SETUP.md)). To reach it from the
phone, leave `MAGNETAR_BIND=127.0.0.1:8787` and publish the port to your tailnet with HTTPS:

```sh
tailscale serve --bg 127.0.0.1:8787     # https://<machine>.<tailnet>.ts.net -> 127.0.0.1:8787
tailscale serve status
```

This needs MagicDNS and HTTPS certificates enabled for the tailnet (admin console, DNS page).
In the app, set the server URL to `https://<machine>.<tailnet>.ts.net` and keep the token.

The app allows cleartext HTTP only to `127.0.0.1` and `localhost`. To use plain
`http://<magicdns-name>:8787` or a `100.x` address instead, you would have to add that host
to `android/app/src/main/res/xml/network_security_config.xml` and rebuild the APK, and bind
the server to its Tailscale IP. HTTPS through `tailscale serve` avoids both. **UNVERIFIED:**
Android 16+ "local network access" rules may affect Tailscale addresses, but loopback is
expected to be exempt.

## Building from source (laptop)

```sh
# Arch's /usr/bin/cargo has no Android std; the rustup toolchain must come first:
export PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865

scripts/build-termux.sh          # dist/magnetar-server-{aarch64,x86_64}-linux-android (cargo-ndk, API 24)
scripts/build-android.sh         # .so via cargo-ndk + Kotlin bindings + APK
cargo build --release -p magnetar-server   # native Linux server
```

You need the rustup stable toolchain with the `aarch64-linux-android` and
`x86_64-linux-android` targets, `cargo-ndk` 4.x, NDK r29, JDK 17+ (27 works with Gradle
9.8.2), and the Android SDK with compileSdk 37. Do not install aws-lc or OpenSSL: TLS is
rustls with ring and webpki-roots. Copying a binary to a phone without a release:
`scp -P 8022 dist/magnetar-server-aarch64-linux-android <phone>:` then
`MAGNETAR_BINARY=~/magnetar-server-aarch64-linux-android bash termux-setup.sh`.
More in [docs/SETUP.md](docs/SETUP.md).

Releases: `scripts/release.sh` prints the plan (dry run), `--build` fills `dist/` and writes
`SHA256SUMS`, and only `--publish` calls `gh release create v<version>[-suffix]`.

## Testing

```sh
scripts/test.sh                  # fmt --check, clippy -D warnings, all tests with Postgres 18, Android build
scripts/test.sh --ignored        # + network tests against real relays and trackers
scripts/test.sh --termux         # + termux-setup.sh inside termux/termux-docker:x86_64
scripts/termux-docker-test.sh --binary dist/magnetar-server-x86_64-linux-android   # with the real server
```

`scripts/test.sh` starts (or creates) the docker container `magnetar-pg` (`postgres:18` on
`127.0.0.1:54329`) and exports `MAGNETAR_TEST_DATABASE_URL`. Every test uses its own schema.

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
- **The phantom process killer** (Android 12+) kills Termux services unless you disable it.
  The setup keeps the process count low (Postgres `max_connections 20`, `io_workers 1`, a
  sqlx pool of 5 or fewer).
- **Loopback is shared by all apps**, which is why there is a token. Anyone who can read
  `~/.config/magnetar/env` in Termux has full API access.
- The Termux server binary has not run on a physical phone yet (**UNVERIFIED**). It was built
  for API 24 and checked only in termux-docker on x86_64.

## Legal

Magnetar indexes metadata that other people publish on public Nostr relays. It does not host,
seed or distribute any content by itself. You are responsible for what you search for,
download and share, and for complying with the laws of your country.

License: MIT (see `Cargo.toml`).
