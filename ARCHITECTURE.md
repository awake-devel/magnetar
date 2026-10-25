# Magnetar — Architecture & shared contracts

Magnetar crawls Nostr relays for NIP-35 torrent events (kind 2003), stores and indexes them,
scores publishers with a web-of-trust, checks swarm liveness, and lets you search, inspect and
download from an Android app. A server (Postgres) and the phone (SQLite cache) both crawl; the
phone syncs what it finds up to the server.

This file is the **contract** between components. Research backing every decision lives in
`docs/research/*.md` (sources, live measurements, gotchas). When code and this file disagree,
fix one of them in the same change.

---

## 0. Decisions log (with research pointers)

| # | Decision | Why / source |
|---|---|---|
| D1 | Rust workspace, edition 2024, `rust-version = "1.94"` (sqlx 0.9 MSRV) | rust-crates.md §1 |
| D2 | `nostr` 0.45.5 for event types/verify/bech32; **no** `nostr-sdk` at runtime (dev-dep interop oracle only) | negentropy.md §1, rust-crates.md §4 |
| D3 | Own thin relay WebSocket client in `core` (tokio-tungstenite, rustls+ring+webpki-roots, 16 MiB max msg) used by crawler **and** sync client | nostr-nips.md §4, negentropy.md §7 |
| D4 | Own NIP-35 parser, lenient (btih alias, `name;size` files, junk after hash, reversed proxy tags…) | nostr-nips.md §2.4 |
| D5 | Ingest = recompute id + verify BIP340 sig, always, on every path (crawler, relay EVENT, batch POST) | nostr-nips.md §1.2 |
| D6 | One row per **event**; torrents grouped by **infohash** for presentation; canonical event per infohash = highest trust, then earliest | nostr-nips.md §2.4 |
| D7 | Kind 2004 accepted only if it references (e/E tag) a known 2003; kind 1111 with `K=2003` ingested as comment too | nostr-nips.md §2.5 |
| D8 | Pagination: `until = min(created_at)` (inclusive, dedup), strictly decreasing, stop on a page with 0 new events; never trust "page < limit" | nostr-nips.md §1.7 |
| D9 | Cursors per (relay, kind): `newest_seen` (forward, overlap 1 h) + `backfill_until` (history not finished) | nostr-nips.md §1.7 |
| D10 | Relay discovery: seed list ∪ NIP-65 lists of publishers ∪ kind-3 legacy maps ∪ tag hints ∪ NIP-66 monitors (D35); own normalizer; oversized lists (>50 r) penalised and not used | wot-relays.md §3, §5 |
| D11 | Postgres search: generated `tsvector` (`simple` on title/files with separator regexp, `english` on description) + `pg_trgm` `<%` word_similarity fallback | rust-crates.md §9 |
| D12 | SQLite search: FTS5 external content, `unicode61 remove_diacritics 2 separators '._-[]()'`, sanitized query tokens | rust-crates.md §5 |
| D13 | NIP-77: own relay side (axum WS + `negentropy` 0.5.2), own initiator on phone; frame limit 60 000; max 500 000 items per NEG-OPEN | negentropy.md §1, §5 |
| D14 | Phone → server sync is **Up-only**; `sync_ids` table never pruned; rows marked synced only on `OK true` (incl. `duplicate:`) | negentropy.md §7 |
| D15 | Batch fallback `POST /api/v1/events/batch` when NEG-ERR / NOTICE / no NEG-MSG in 10 s | negentropy.md §7 |
| D16 | Liveness: own UDP (BEP 15, 74/batch, positional, truncation = unanswered) + HTTP scrape (BEP 48, lenient bencode); BEP 33 DHT scrape only when no tracker answered; aggregate = max over trackers | bittorrent.md §0 |
| D17 | Liveness TTL: alive 6 h, dead 24 h, error 1 h; unknown ≠ dead | bittorrent.md §8 |
| D18 | Trust: score 0–100 per publisher + per-event PoW bonus; factors stored for explanation; depth-2 WoT; default threshold 20; bridges (proxy tag) exempt from BURST | wot-relays.md §9 |
| D19 | PoW effective bits = min(actual, committed target); weighted lightly (max +25) | nostr-nips.md §3 |
| D20 | Server binds 127.0.0.1 only; **every** endpoint except `/health` needs `Authorization: Bearer <token>` (other Android apps share loopback); `/relay` WS accepts `?token=` | termux.md §1.3 |
| D21 | reqwest 0.13 with `rustls-no-provider` + preconfigured rustls(ring, webpki-roots) — never platform-verifier (breaks on Android w/o JNI) | rust-crates.md §7 |
| D22 | sqlx runtime queries only (`query_as` + `FromRow`, `QueryBuilder`), no `query!` macros → builds without a DB (Termux/cross) | rust-crates.md §8 |
| D23 | `rusqlite = 0.39` (bundled, FTS5) — newer clashes with sqlx's `links = sqlite3` | rust-crates.md §5 |
| D24 | Termux: Postgres 18.6 via `pkg`, socket at `$PREFIX/tmp`, runit services, binary cross-compiled with cargo-ndk API 24; tests run against docker `postgres:18` | termux.md |
| D25 | Notifications: ntfy JSON publish (root URL), one message per poll for all saved searches (D34; was one per search); local polling + Android notification fallback | termux.md §4 |
| D26 | The Android app never parses server JSON: the `ffi` crate contains the API client (from `core::api`) and returns uniffi records for both online and offline paths | this doc §6 |
| D28 | librqbit 9.0.1 `rust-tls` still pulls **aws-lc-rs** (its `sha1-ring` feature maps to aws-lc, and its reqwest uses default rustls + platform-verifier). Accepted for the `downloads` feature only; it cross-compiles with cargo-ndk (android.md §4). Because rustls then has two providers, our code must always use `builder_with_provider(ring)` and call `http::install_crypto_provider()` at startup. librqbit's own HTTPS tracker requests may fail on Android (platform verifier without JNI init): UDP trackers + DHT still work | verified via `cargo tree -i aws-lc-rs` |
| D29 | `crates/core/src/store.rs` is the authoritative Store trait (§4 is a summary) | |
| D27 | Downloads: librqbit on server; on phone see §8 (spike result recorded there) with fallback = `ACTION_VIEW magnet:` intent | android.md |
| D30 | Relay selection per run (`crawler::select_relays`): ≤ `max_relays`, no duplicates, disabled/backed-off skipped. Up to `max_relays − explore_relays` slots for **proven** relays (`events_found > 0`) by yield then NIP-65 popularity; every other slot rotates through zero-yield relays ordered by `next_try_at` (0 = never tried, last ok run for reachable ones, end of backoff for failing ones), ties seeds in config order then most listed; leftover slots back to proven relays. Seeds get no fixed slots: they lead on a fresh install (all ties) and compete by yield afterwards, so seeds past the cap (phone: 12 defaults vs 8) are rotated in instead of cut forever, and proven non-seed relays are not crowded out. Stored non-seed/non-manual rows must still pass `normalize_relay(_, false)`, which now also rejects private TLDs (`.home`, `.lan`, `.internal`, `.arpa`, …; checked against the IANA root zone list 2026102500) after `ws://apollo.home:3355` arrived via a NIP-65 list | ffc4ec5 review; `tests/crawler.rs` selection tests |
| D31 | Correction to D30 (rotation): proven relays below the top `max_relays − explore_relays` by lifetime `events_found` are no longer filler after the zero-yield list (which never runs out: ok zero-yield relays requeue at `now`, failing ones after backoff, discovery keeps adding rows), they join the same `next_try_at` rotation (ties: higher yield, then seed order, then most listed). Lifetime yield only grows when a relay is crawled, so a relay left below the cutoff could never climb back: with 5 slots/2 explore, proven relays at 100/50/10/1 events and 10 ok zero-yield relays, the 4th was picked 0 times in 50 passes; it now gets a turn every 5–6 passes. Never-tried relays (`next_try_at` 0) still come first, so new discoveries are not delayed by the tail. The top yielders keep fixed slots (no per-crawl or recent-yield ranking: that needs history the store does not keep) | swarm/explore review; `tests/crawler.rs::proven_relays_below_the_cutoff_still_rotate` |
| D32 | Continuous crawling: `MAGNETAR_CRAWL_INTERVAL_SECS=0` runs passes back to back with a fixed 30 s pause (120 s after a failed pass or one with no relay to try, so an instant pass never hot-spins); 1–59 is raised to 60, default stays 900 for phone servers. Each pass keeps the 600 s budget, relay backoff is unchanged (`next_try_at` is absolute time, 300 s doubling to 24 h, so a failing relay is skipped by `select_relays` however often passes run; what counts as failing is in D34). Indexer metadata and the saved-search notifier run at most every 15 min in every mode (the notifier is watermarked, so skipped polls only batch hits; the ntfy.sh limit is kept by D34's one message per poll). 30 s rather than less: a steady-state pass already reconnects to up to 30 public relays and re-reads the 1 h forward overlap, so a shorter pause mostly adds connection churn against relay rate limits while kind 2003 arrives slowly | measured: first pass on the laptop took 457 s (45 942 inserted, 10 relays ok) |
| D33 | `/relay` connections take a live broadcast receiver only while they have an open REQ, and drop it at the last CLOSE. Before, every connection subscribed at connect, so an upload-only phone sync (NEG + ~4.4k EVENTs, no REQ) sat behind the 1024-slot channel while its own and the crawler's ingests filled it and logged dozens of "subscriber lagged" warnings. Nothing was lost: lag only skips live pushes to open REQs, every event is already stored. Real lag now warns once per connection and the total is in the close line (`live_dropped`) | server.log 2026-10-25: 579 lag warnings during one upload connection (`accepted=2267 duplicates=2141`); `crates/core/src/sync.rs` sends no REQ |
| D34 | Soft crawl trouble that still backs off: a final CLOSED (after the one retry) whose prefix is `blocked`, `rate-limited`, `restricted` (NIP-01) or `auth-required` (NIP-42), case-insensitive, records the visit as a failure even if it inserted events, and nothing more is asked of that relay in the run; a page unanswered for the full 20 s `PAGE_TIMEOUT` is a failure when the visit inserted nothing. A timeout cut short by the crawl deadline counts as budget, not timeout. `error:`/`invalid:`/`unsupported:` CLOSEDs stay soft: they reject one filter (strfry: `ERROR: bad req: unrecognised filter item`), not us. Before, every CLOSED or timeout reset `consecutive_failures` and `next_try_at = now`, so continuous mode (D32) revisited a refusing relay every ~30 s. The saved-search notifier sends **one** ntfy message per poll (`notify::format_digest`; a lone matching search keeps the `format_hits` form): per-search messages exceeded ntfy.sh's 250/day with 3 always-matching searches (3 × 96 polls), now at most 96/day. Supersedes D25's "one message per saved search per poll" | NIP-01 (`CLOSED` prefixes) and NIP-42 (`auth-required:` on REQ), nostr-protocol/nips master, read 2026-10-25; nostr-nips.md §1.5; termux.md §4 (250/day) |
| D35 | NIP-66 relay discovery (server only, every 6 h): kind 10166 from monitor relays → announced monitor set; kind 30166 **by those authors only**, last 7 days, pages of 500 (D8); verify each; `d` normalized with `normalize_relay(url,false)`; drop non-clearnet (`n` ≠ clearnet, .onion/.i2p/.loki), hex-pubkey `d`, and any relay that **any** monitor tags `R auth` / `R payment`; insert ≤ 2000 new per run as source `nip66`, most monitors first. `N` tags ignored (crawler needs neither NIP-50 nor 77). Defaults relaypag.es, monitorlizard.nostr1.com, relay.nostr.watch, nos.lol, relay.damus.io | nip66.md |
| D36 | NIP-66 trust (amends D35): anyone can publish a 10166, so announcements alone are not trusted. (a) A built-in **trusted monitor** list (`DEFAULT_TRUSTED_MONITORS`: every announced monitor with > 100 30166 in 7 days on 2026-10-25; env `MAGNETAR_NIP66_TRUSTED`) is always queried in its **own** `authors` filter, before and with a page budget separate from the other announced ("open") monitors, so fresh sybil 10166s cannot evict it (ordering by 10166 recency would: the real fleet's are 80–130 days old). (b) Open monitors: ≤ 250 per filter, ranked by monitor relays carrying the announcement then recency, ≤ 10 pages per relay. (c) An open monitor's `R auth`/`payment` only counts for relays no trusted monitor reports. (d) Relays reported by open monitors only rank after trusted-reported ones and are capped at 100 inserts per run. (e) Each 30166 is verified once across relays and reduced to (monitor, created_at, verdict) right after its page: no event bodies are held during a run | nip66.md §6 |
| D37 | Docker deployment: root `Dockerfile` (multi-stage, native per-arch build, no cross/QEMU; builder `rust:1.99.0-slim-trixie`, runtime `debian:trixie-slim`, both pinned by index digest; cargo-chef dependency layer; release binary stripped) and `compose.yml` (`db` = `postgres:18` + named volume at `/var/lib/postgresql` + `pg_isready` healthcheck; `magnetar` = `ghcr.io/awake-devel/magnetar:${MAGNETAR_VERSION:-latest}`/`build: .`, `depends_on: service_healthy`, both `restart: unless-stopped`, no host service units). Image: non-root uid/gid 10001, `/data` volume (`MAGNETAR_DOWNLOAD_DIR=/data/downloads`, `XDG_STATE_HOME=/data/state` → librqbit state `/data/state/magnetar/downloads`, so no new state-dir variable), `MAGNETAR_BIND=0.0.0.0:8787` inside the container while compose publishes `${MAGNETAR_PUBLISH:-127.0.0.1:8787}` (D20 still holds at the host), `ENTRYPOINT magnetar-server` + `CMD serve` so `compose run magnetar crawl-once` works (`serve` migrates first), `HEALTHCHECK` = bash `/dev/tcp` GET `/health` requiring `"db":true` (no curl in the image). glibc runtime rather than musl/distroless: same libc as the builder, no musl malloc slowdown, a shell for the healthcheck and `exec`; CA bundle copied from the builder for librqbit's platform-verifier HTTPS and later TMDB. compose fails fast via `${VAR:?}` when `POSTGRES_PASSWORD`/`MAGNETAR_API_TOKEN` are unset; an existing postgres:18 volume is adopted with an `external: true` override (DOCKER.md). Tailscale is an optional `tailscale` profile (userspace sidecar, `TS_SERVE_CONFIG` → `http://magnetar:8787`). New `MAGNETAR_DB_POOL` (sqlx max connections, default 10, ≥ 1): the old fixed cap of 5 was the Termux process budget (D24); 10 stays far below Postgres' default `max_connections` 100 | docker build + compose run on the laptop (x86_64), 2026-10-25. arm64 not verified: no emulation here, and no image is published yet (the multi-arch publish job is D39, planned); until then DOCKER.md says to build from a full checkout |
| D37a | Docker hardening after review: the magnetar service lists the server variables by name under `environment:` instead of `env_file: .env`, so `TS_AUTHKEY` (sidecar only) and `POSTGRES_PASSWORD` (already in `DATABASE_URL`) never reach the server container; a bare key is unset in the container when `.env` lacks it (which also masks an image `ENV` of that name, so `RUST_LOG` gets `${RUST_LOG:-info}` and the image-set `MAGNETAR_BIND`/`MAGNETAR_DOWNLOAD_DIR`/`XDG_STATE_HOME` are not listed), and extra variables go in `compose.override.yml`. `.gitattributes` forces LF for `*.sh` and `docker/**`, and the Dockerfile strips CRs from the healthcheck script anyway (a CRLF shebang left the container unhealthy). `.env` is `chmod 600` in the quick start | `docker compose config` + `docker inspect` of a magtest stack, CRLF healthcheck built and reported healthy, 2026-10-25 |
| D38 | Native Linux deployment without Docker: `scripts/build-linux.sh <x86_64\|aarch64>` builds a **static musl** `magnetar-server` inside `rust:1.99-alpine` (musl is Alpine's native libc, so ring and librqbit's aws-lc (D28) compile with plain gcc and link statically; `--native` uses the host's cargo + `musl-gcc`); native per-arch only, aarch64 on an arm64 CI runner, no QEMU. Output `dist/magnetar-server-<arch>-linux` is rejected unless it has no ELF interpreter and no `NEEDED`. `scripts/install-linux.sh` (POSIX sh: dash, busybox ash) installs as the user into `~/.local` from `MAGNETAR_BINARY` or a release asset checked against `SHA256SUMS` (missing entry = abort), writes `~/.config/magnetar/server.env` (600, random token, existing keys kept), and takes Postgres from `DATABASE_URL`, else a `postgres:18` container (`--restart unless-stopped`, named volume, 127.0.0.1:54320), else prints distro steps. `scripts/magnetar-ctl` (generalised laptop `magnetar-host`) runs the server as a `setsid nohup` process with pid/log in `~/.local/state/magnetar`; `tailscale serve` only with `MAGNETAR_TAILSCALE_SERVE=1`. No systemd/OpenRC/cron entries are installed: docs/LINUX.md shows optional examples | verified 2026-10-25: x86_64 build `static-pie linked`, install + up/health/restart/down in debian:bookworm and alpine:3.22, docker-DB path, release download + checksum mismatch from a local mirror; aarch64 not built locally |
| D38a | Review fixes to the native Linux path. `magnetar-ctl update`: the caller's `MAGNETAR_VERSION` wins over the one `server.env` saved at install (else saved, else `latest`), so a pinned prerelease moves only when asked. The pid file is trusted only while `/proc/<pid>/cmdline` is `<bin> serve` (argv, not `/proc/<pid>/exe`, which reads `(deleted)` after `update` replaced the binary); a stale pid is dropped, never signalled. State dir 700, `server.log` 600 (debug logs carry `?token=` URIs; librqbit session lives there). `tailscale serve` uses `--set-path=/`; `up` will not replace a foreign handler at `/` of that port, `stop-all` removes only `/` and only if it still proxies to this server. `build-linux.sh --native` stops early when rustc has no std for `<arch>-unknown-linux-musl` (Alpine's `rust` package has only `<arch>-alpine-linux-musl`): rustup is required. Installer refuses a fresh `server.env` against an existing DB container (unknown password) unless `MAGNETAR_DB_PASSWORD` is given; LINUX.md lists what a second instance really needs (`XDG_STATE_HOME`, since the server ignores `MAGNETAR_STATE_DIR`) | verified 2026-10-25 on x86_64: update version precedence (stub installer), stale pid owned by a `sleep` left alone by up/down, stop after the binary was replaced, modes 700/600, `serve status --json` parsing against a stub `tailscale` (own / foreign / other-port fixtures); real `tailscale serve --set-path` not run (production serve config untouched); Alpine `rust` guard in alpine:3.22; DB guard with a throwaway container |

---

## 1. Repository layout

```
Cargo.toml                 workspace (all dependency versions pinned here)
Dockerfile, compose.yml    server image + Postgres stack, .env.example, docker/ (D37, docs/DOCKER.md)
crates/core/               magnetar-core   (lib)  — no DB driver, no axum; compiles for Android
crates/server/             magnetar-server (bin + lib) — Postgres, axum API, relay endpoint, daemons
crates/ffi/                magnetar-ffi    (cdylib + lib) — SQLite store + uniffi API for Android
crates/uniffi-bindgen/     uniffi-bindgen  (bin)
android/                   Gradle project, app module `app`, package `app.magnetar`
scripts/                   termux-setup.sh, termux-ctl.sh, build-android.sh, build-termux.sh, test.sh
docs/research/             research notes + fixtures (real relay events)
```

### Build environment (this laptop)
- `/usr/bin/cargo` (Arch) has no Android std. **All Android builds:**
  `PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865`.
- Tests needing Postgres read `MAGNETAR_TEST_DATABASE_URL`
  (local: `postgres://magnetar:magnetar@127.0.0.1:54329/magnetar`, docker `postgres:18`).
  Each test creates its own schema `t_<random>` and drops it, so tests run in parallel.
  If the variable is unset, DB tests print `SKIPPED (no MAGNETAR_TEST_DATABASE_URL)` and pass;
  `scripts/test.sh` always sets it, so CI/"done" means they actually ran.

---

## 2. `magnetar-core` modules (owner in brackets = build agent)

```
core/src/
  lib.rs            re-exports
  error.rs          Error enum (thiserror)                                   [contracts]
  model.rs          domain types (§3)                                         [contracts]
  store.rs          Store trait (§4)                                          [contracts]
  query.rs          SearchQuery / Sort / filters (§5)                         [contracts]
  api.rs            HTTP DTOs + ApiClient (reqwest) (§6)                      [trust-sync]
  nip35.rs          parse kind 2003 → ParsedTorrent; comments                 [nostr]
  ingest.rs         verify + classify raw events → IngestItem                 [nostr]
  pow.rs            NIP-13                                                     [nostr]
  magnet.rs         build/parse magnet URIs, infohash normalisation            [nostr]
  relayurl.rs       relay URL normalizer                                       [nostr]
  relay.rs          RelayConn: WS client (REQ/EOSE/CLOSED/NOTICE/AUTH/OK, NEG-*) [nostr]
  crawler.rs        Crawler<S: Store>: discovery, pagination, cursors          [nostr]
  nip66.rs          NIP-66 relay discovery from monitor events (D35)            [nostr]
  wot.rs            follow/mute/report extraction from kinds 3/10000/30007/1984/10002 [trust-sync]
  trust.rs          score computation (pure) + TrustEngine<S>                  [trust-sync]
  sync.rs           negentropy initiator (Up) + batch fallback, SyncSource trait [trust-sync]
  notify.rs         ntfy publisher + SavedSearch evaluation helpers            [trust-sync]
  liveness/mod.rs   LivenessChecker: group-by-tracker, TTL, aggregation        [liveness]
  liveness/bencode.rs  lenient decoder                                         [liveness]
  liveness/udp.rs   BEP 15                                                     [liveness]
  liveness/http.rs  BEP 48                                                     [liveness]
  liveness/dht.rs   BEP 33 lookup + Bloom256                                   [liveness]
  http.rs           shared reqwest Client builder (D21)                        [contracts]
```

Async trait methods use `async-trait` (dyn-compatible, `Send`).

---

## 3. Domain model (`core::model`) — exact Rust

All hex is lowercase. Times are unix seconds `i64`.

```rust
pub type Hex32 = String;   // 64 hex chars (event id, pubkey)
pub type InfoHash = String; // 40 hex chars, v1

/// A verified raw nostr event as stored (any kind we keep).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawEvent {
    pub id: Hex32, pub pubkey: Hex32, pub created_at: i64, pub kind: u16,
    pub tags: Vec<Vec<String>>, pub content: String, pub sig: String,
}
// RawEvent::from_json(&str) -> Result<RawEvent>   (no verification)
// RawEvent::verify(&self) -> Result<()>          (id recompute + schnorr, via nostr crate)
// RawEvent::to_json(&self) -> String             (NIP-01 wire form)

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentFile { pub path: String, pub size: Option<u64> }

/// Result of parsing a kind 2003 (D4). Produced by nip35::parse_torrent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ParsedTorrent {
    pub event_id: Hex32, pub pubkey: Hex32, pub created_at: i64,
    pub infohash: InfoHash,
    pub infohash_v2: Option<String>,          // 64 hex if an x tag was 64-hex
    pub title: String,                        // title | first file | infohash
    pub description: String,                  // raw content, capped 64 KiB
    pub files: Vec<TorrentFile>,              // capped 5000
    pub total_size: Option<u64>,              // sum(files) if all known and >0, else `size` tag
    pub trackers: Vec<String>,                // validated udp/http/https/wss, deduped, order kept, cap 30
    pub category: Category,                   // derived (§3.1)
    pub category_path: Vec<String>,           // tcat split on ',' or first 3 t tags
    pub hashtags: Vec<String>,                // t values lowercased, deduped
    pub external_ids: Vec<(String, String)>,  // i tags split at FIRST ':'
    pub source_url: Option<String>,           // proxy tag URL (either arg order)
    pub published_at: Option<i64>,
    pub pow_bits: u8,                         // effective (min(actual, committed))
    pub pow_committed: Option<u8>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Category { Video, Audio, Application, Game, Porn, Anime, Book, Picture, Other }
// Category::as_str()/from_str() use the snake_case names above.

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub event_id: Hex32, pub pubkey: Hex32, pub created_at: i64, pub kind: u16, // 2004 or 1111
    pub torrent_event_id: Hex32, pub content: String,
}

/// What ingest decided about one raw event.
pub enum IngestItem {
    Torrent(ParsedTorrent, RawEvent),
    Comment(Comment, RawEvent),
    ContactList(RawEvent),          // kind 3
    MuteList(RawEvent),             // kind 10000, or 30007 with d="2003"
    Report(RawEvent),               // kind 1984
    RelayList(RawEvent),            // kind 10002
    Profile(RawEvent),              // kind 0 (only stored as "has profile" + name)
}
pub enum Rejection { BadJson, BadId, BadSig, FutureDated, UnsupportedKind, NoInfohash, OrphanComment, TooLarge }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LivenessSource { Udp, Http, Dht, DhtValues, None }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Liveness {
    pub infohash: InfoHash,
    pub seeders: Option<u32>, pub leechers: Option<u32>, pub completed: Option<u32>,
    pub dht_peers_est: Option<u32>,
    pub source: LivenessSource,
    pub trackers_ok: u16, pub trackers_failed: u16,
    pub checked_at: i64,
    pub expires_at: i64,           // checked_at + TTL (D17)
}
impl Liveness { pub fn is_dead(&self, now: i64) -> bool; pub fn best_peers(&self) -> Option<u32>; }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrustFactor { pub code: String, pub delta: i32, pub detail: String, pub evidence: Vec<String> }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrustScore {
    pub pubkey: Hex32, pub score: u8, pub factors: Vec<TrustFactor>,
    pub root: Option<Hex32>, pub computed_at: i64, pub graph_version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelayInfo {
    pub url: String, pub source: String,          // "seed" | "nip65" | "kind3" | "hint" | "nip66" | "manual"
    pub enabled: bool, pub last_ok: Option<i64>, pub last_err: Option<String>,
    pub consecutive_failures: u32, pub next_try_at: i64,
    pub events_found: u64, pub supports_neg: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cursor { pub relay: String, pub kind: u16, pub newest_seen: Option<i64>, pub backfill_until: Option<i64>, pub backfill_done: bool }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideAction { Block, Trust }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedSearch { pub id: i64, pub name: String, pub query: SearchQuery, pub notify: bool,
                         pub last_checked: Option<i64>, pub last_seen_created_at: Option<i64> }
```

### 3.1 Category mapping (in `nip35::category_of`)
Lower-case the first `tcat` element (else first `t`), then: `video|movie|tv|hd|uhd|4k|series` → Video;
`audio|music|flac|lossless|audio-book|audiobook` → Audio; `application|software|windows|mac|unix|linux|android|ios` → Application;
`game|games|pc|psx|xbox|wii` → Game; `porn|xxx|adult` → Porn; `anime` → Anime; `book|e-book|ebook|comic|manga` → Book;
`picture|pictures|image` → Picture; anything else → Other. YGG French values (`filmvidéo`, `série`, …) mapped by
substring (`film`/`série`/`vidéo` → Video, `musique` → Audio, `jeu` → Game, `logiciel` → Application, `ebook`/`livre` → Book).

### 3.2 Torrent presentation types (returned by search / detail)
```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentSummary {
    pub infohash: InfoHash, pub event_id: Hex32,            // canonical event
    pub title: String, pub category: Category, pub total_size: Option<u64>,
    pub file_count: u32, pub created_at: i64, pub pubkey: Hex32, pub npub: String,
    pub seeders: Option<u32>, pub leechers: Option<u32>, pub dead: Option<bool>, // None = unknown
    pub trust: u8, pub pow_bits: u8, pub publisher_count: u32,
    pub source_url: Option<String>, pub rank: f32,
    pub pinned: bool, pub hidden: bool,                    // phone-local flags (server always false)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentDetail {
    pub summary: TorrentSummary, pub description: String,
    pub files: Vec<TorrentFile>, pub trackers: Vec<String>, pub hashtags: Vec<String>,
    pub category_path: Vec<String>, pub external_ids: Vec<(String,String)>,
    pub magnet: String, pub liveness: Option<Liveness>, pub trust: TrustScore,
    pub events: Vec<EventRef>,            // every event that published this infohash
    pub comments: Vec<Comment>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventRef { pub event_id: Hex32, pub pubkey: Hex32, pub npub: String, pub created_at: i64, pub title: String, pub trust: u8 }
```

---

## 4. `Store` trait (`core::store`) — implemented by `PgStore` (server) and `SqliteStore` (ffi)

```rust
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    // ingest — idempotent; returns true if newly inserted. Implementations MUST NOT verify again
    // (callers pass only items produced by core::ingest which already verified).
    async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool>;
    async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool>;
    /// replaceable kinds 0/3/10000/30007/10002: keep newest per (pubkey, kind) (lowest id on tie);
    /// replace derived rows (follows / mutes / relay_lists / profiles) in one transaction.
    async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool>;
    async fn put_report(&self, raw: &RawEvent) -> Result<bool>;
    async fn has_event(&self, id: &str) -> Result<bool>;
    async fn known_torrent_event(&self, id: &str) -> Result<bool>;   // for orphan-comment check
    async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>>;

    // relays + cursors
    async fn upsert_relay(&self, url: &str, source: &str) -> Result<()>;      // never overwrites a manual/disabled flag
    async fn list_relays(&self) -> Result<Vec<RelayInfo>>;
    async fn record_relay_result(&self, url: &str, ok: bool, err: Option<&str>, found: u64, now: i64) -> Result<()>;
    async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()>;
    async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>>;
    async fn put_cursor(&self, c: &Cursor) -> Result<()>;
    async fn publishers(&self, limit: usize) -> Result<Vec<String>>;          // distinct pubkeys with ≥1 torrent
    async fn relay_lists_for(&self, pubkeys: &[String]) -> Result<Vec<(String, String, bool, bool)>>; // (pubkey,url,read,write)
    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool>;

    // search / detail
    async fn search(&self, q: &SearchQuery, now: i64) -> Result<SearchPage>;
    async fn torrent_detail(&self, infohash: &str) -> Result<Option<TorrentDetail>>;

    // liveness
    async fn liveness_due(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>>; // (infohash, trackers)
    async fn put_liveness(&self, l: &Liveness) -> Result<()>;
    async fn get_liveness(&self, infohash: &str) -> Result<Option<Liveness>>;

    // trust graph
    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>>;
    async fn followers_among(&self, targets: &[String], among: &[String]) -> Result<Vec<(String, Vec<String>)>>; // target -> which of `among` follow it
    async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>>;   // (muter, target)
    async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>>; // (reporter, target, type)
    async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats>;
    async fn put_trust(&self, s: &TrustScore) -> Result<()>;                   // also refreshes denormalised trust on torrents
    async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>>;
    async fn graph_version(&self) -> Result<i64>;
    async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()>;
    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>>;

    // settings (key/value, JSON values)
    async fn get_setting(&self, key: &str) -> Result<Option<String>>;
    async fn set_setting(&self, key: &str, value: &str) -> Result<()>;

    // saved searches
    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>>;
    async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64>;          // id 0 = insert
    async fn delete_saved_search(&self, id: i64) -> Result<()>;
    async fn stats(&self) -> Result<StoreStats>;
}

pub struct PublisherStats { pub first_seen: Option<i64>, pub torrent_count: u64, pub has_profile: bool,
    pub has_relay_list: bool, pub max_per_hour: u32, pub is_bridge: bool, pub wot_comments: u32 }
pub struct StoreStats { pub torrents: u64, pub events: u64, pub publishers: u64, pub comments: u64,
    pub relays: u64, pub unsynced: u64, pub db_bytes: u64 }
```
`SqliteStore` adds phone-only inherent methods (not in the trait): pin/hide flags, `sync_ids`,
`unsynced_ids`, `mark_synced`, `cleanup(cap_bytes)`, `touch(infohash)` for LRU.

Setting keys: `trust.root` (hex), `trust.threshold` (int, default 20), `crawl.seed_relays` (JSON array),
`liveness.enabled` (bool), `ntfy.url`, `ntfy.topic`.

---

## 5. Search query (`core::query`)

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SearchQuery {
    pub text: String,                     // free text; empty = browse
    pub categories: Vec<Category>,        // empty = all
    pub min_size: Option<u64>, pub max_size: Option<u64>,
    pub since: Option<i64>, pub until: Option<i64>,     // created_at range
    pub min_seeders: Option<u32>,
    pub hide_dead: bool,
    pub min_trust: Option<u8>,            // None = use setting trust.threshold
    pub sort: Sort, pub descending: bool, // default Relevance if text non-empty else Date; descending = true
    pub limit: u32, pub offset: u32,      // limit default 50, max 200
    pub include_hidden: bool,             // phone only
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort { #[default] Relevance, Date, Size, Seeders }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchPage { pub hits: Vec<TorrentSummary>, pub total: u64, pub source: String /* "server" | "local" */ }
```
Semantics (both stores): results are **grouped by infohash** (canonical event); blocked publishers are
always excluded; `hide_dead` drops rows whose liveness is fresh and `best_peers()==0`; unknown is kept;
`min_seeders` treats unknown as 0 except when it is None; `sort=seeders` puts unknown last.
`core::query::fts5_match(text) -> Option<String>` and `pg_tsquery(text)` sanitize text (D11, D12).

---

## 6. HTTP API (`server`) and `core::api::ApiClient`

Base `http://127.0.0.1:8787`. JSON in/out. Errors: `{ "error": "<code>", "message": "..." }` with 4xx/5xx.
Auth: `Authorization: Bearer <MAGNETAR_API_TOKEN>` on everything except `GET /health`.

| Method & path | Body / query | Response |
|---|---|---|
| `GET /health` | | `{"status":"ok","version":"x.y.z","db":true}` |
| `GET /api/v1/search` | `SearchQuery` as query string: `q, cat` (repeatable/comma), `min_size, max_size, since, until, min_seeders, hide_dead, min_trust, sort, desc, limit, offset` | `SearchPage` |
| `POST /api/v1/search` | `SearchQuery` JSON | `SearchPage` |
| `GET /api/v1/torrents/{infohash}` | | `TorrentDetail` / 404 |
| `POST /api/v1/torrents/{infohash}/liveness` | | `Liveness` (forces a check now) |
| `POST /api/v1/events/batch` | `{"events":[RawEvent...]}` (≤ 500) | `{"results":[{"id":..,"accepted":bool,"duplicate":bool,"message":".."}]}` |
| `GET /api/v1/stats` | | `StoreStats` + `{"crawler":{"running":bool,"last_run":i64,"last_report":CrawlReport,"passes":u64,"last_error":str,"next_run":i64}}` |
| `GET /api/v1/relays` / `PUT /api/v1/relays` | `{"url","enabled"}` | `[RelayInfo]` |
| `GET /api/v1/trust/{pubkey}` | | `TrustScore` |
| `GET/PUT /api/v1/settings/trust` | `{"root": npub-or-hex or null, "threshold": u8}` | same |
| `PUT /api/v1/overrides/{pubkey}` | `{"action":"block"|"trust"|null}` | 204 |
| `GET/POST /api/v1/saved-searches`, `DELETE /api/v1/saved-searches/{id}` | `SavedSearch` | `[SavedSearch]` / `SavedSearch` |
| `GET/POST /api/v1/downloads`, `POST /api/v1/downloads/{id}/{pause|resume}`, `DELETE /api/v1/downloads/{id}` | `{"magnet": ".."}` | `[DownloadInfo]` |
| `GET /relay` (WebSocket, `?token=`) | NIP-01 REQ/EVENT/CLOSE + NIP-77 NEG-* | |

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DownloadInfo { pub id: u64, pub name: String, pub infohash: InfoHash, pub state: String /* "initializing"|"live"|"paused"|"error"|"finished" */,
    pub progress: f32, pub downloaded: u64, pub total: u64, pub down_bps: u64, pub up_bps: u64, pub peers: u32, pub error: Option<String>, pub output_dir: String }
pub struct BatchResult { pub id: Hex32, pub accepted: bool, pub duplicate: bool, pub message: String }
```
`core::api::ApiClient::new(base_url, token)` exposes one async method per row above, returning
the core types. Relay endpoint: REQ supports `ids, authors, kinds, since, until, limit (≤ 500), #e, #x, #t`;
EVENT accepts kinds 0,3,1984,2003,2004,1111,10000,10002,30007 after `ingest` (D5); `OK` messages use
standard prefixes (`duplicate:`, `invalid:`, `blocked:`).

---

## 7. Sync (phone → server), `core::sync`

```rust
#[async_trait::async_trait]
pub trait SyncSource: Send + Sync {
    async fn neg_items(&self) -> Result<Vec<(u64, [u8; 32])>>;   // from sync_ids (all, incl. evicted bodies)
    async fn load_events(&self, ids: &[String]) -> Result<Vec<RawEvent>>;
    async fn unsynced(&self, limit: usize) -> Result<Vec<RawEvent>>;
    async fn mark_synced(&self, ids: &[String]) -> Result<()>;
    async fn record_failure(&self, id: &str, reason: &str) -> Result<()>;
}
pub struct SyncReport { pub method: String /* "negentropy"|"batch" */, pub have: u64, pub uploaded: u64,
                        pub duplicates: u64, pub failed: u64, pub need_ignored: u64, pub error: Option<String> }
pub async fn sync_up<S: SyncSource>(src: &S, relay_ws_url: &str, api: &ApiClient) -> Result<SyncReport>;
```
Flow per D13–D15. Filter `{"kinds":[2003,2004,1111]}`. Upload window 50. Then the phone store's
`cleanup(cap_bytes)` evicts **only** `synced=1 AND pinned=0` torrent bodies by LRU (`last_access`)
until under cap; `sync_ids` rows are kept. Unsynced rows are never deleted (unit-tested invariant).

---

## 8. Android app & ffi

### 8.1 ffi API (uniffi 0.32 proc-macro, package `app.magnetar.ffi`)
```rust
#[derive(uniffi::Object)] pub struct Magnetar { /* SqliteStore, tokio Runtime, Config */ }
#[uniffi::export(async_runtime = "tokio")]
impl Magnetar {
    #[uniffi::constructor] pub fn open(db_path: String, config: FfiConfig) -> Result<Arc<Self>, FfiError>;
    pub fn set_config(&self, config: FfiConfig) -> Result<(), FfiError>;
    pub async fn search(&self, query: FfiSearchQuery, prefer_server: bool) -> Result<FfiSearchPage, FfiError>; // server first, local fallback; page.source tells which
    pub async fn detail(&self, infohash: String, prefer_server: bool) -> Result<Option<FfiTorrentDetail>, FfiError>;
    pub async fn crawl_once(&self, max_seconds: u32) -> Result<FfiCrawlReport, FfiError>;
    pub async fn sync_now(&self) -> Result<FfiSyncReport, FfiError>;
    pub fn cleanup(&self) -> Result<FfiCleanupReport, FfiError>;
    pub async fn check_liveness(&self, infohash: String) -> Result<Option<FfiLiveness>, FfiError>;
    pub fn set_pinned(&self, infohash: String, pinned: bool) -> Result<(), FfiError>;
    pub fn set_hidden(&self, infohash: String, hidden: bool) -> Result<(), FfiError>;
    pub async fn block_publisher(&self, pubkey: String, blocked: bool) -> Result<(), FfiError>; // local + server if reachable
    pub fn saved_searches(&self) -> Result<Vec<FfiSavedSearch>, FfiError>;
    pub fn save_search(&self, s: FfiSavedSearch) -> Result<i64, FfiError>;
    pub fn delete_saved_search(&self, id: i64) -> Result<(), FfiError>;
    pub async fn poll_saved_searches(&self) -> Result<Vec<FfiSavedSearchHit>, FfiError>; // new results since last poll
    pub fn stats(&self) -> Result<FfiStats, FfiError>;
    pub async fn server_health(&self) -> Result<bool, FfiError>;
    // downloads (§8.3)
    pub async fn download_start(&self, magnet: String, on_server: bool) -> Result<FfiDownload, FfiError>;
    pub async fn downloads(&self, on_server: bool) -> Result<Vec<FfiDownload>, FfiError>;
    pub async fn download_action(&self, id: u64, action: String, on_server: bool) -> Result<(), FfiError>;
}
#[uniffi::export] pub fn npub_encode(hex: String) -> Result<String, FfiError>;
#[uniffi::export] pub fn pubkey_decode(input: String) -> Result<String, FfiError>; // npub/hex/nprofile -> hex
#[uniffi::export] pub fn build_magnet(infohash: String, name: Option<String>, trackers: Vec<String>) -> String;
#[uniffi::export] pub fn version() -> String;
```
`Ffi*` records mirror §3/§5 types 1:1 with uniffi-friendly fields (`u64` sizes, `Option`, enums as uniffi
enums; tuples become `FfiKv{key,value}`). `FfiConfig { server_url: Option<String>, api_token: Option<String>,
root_pubkey: Option<String>, trust_threshold: u8, cache_cap_mb: u32, seed_relays: Vec<String>, download_dir: String }`.
`FfiError` variants: `Db{msg}`, `Network{msg}`, `Invalid{msg}`, `Server{status: u16, msg}`, `Other{msg}`
(never a field named `message`).

### 8.1b Android toolchain (VERIFIED in android.md §1–3)
JDK 27 (system) + Gradle wrapper **9.8.2** + AGP **9.4.1** (built-in Kotlin: do NOT apply
`org.jetbrains.kotlin.android`) + `org.jetbrains.kotlin.plugin.compose` **2.4.21**; compileSdk **37**
(Compose BOM 2026.09.00 requires it), targetSdk 36, minSdk 26; Java/JVM target 17 via
`compileOptions` + `kotlin { compilerOptions { jvmTarget = JVM_17 } }` (no `jvmToolchain`).
Deps: compose-bom 2026.09.00, material3, activity-compose 1.13.0, lifecycle-*-compose 2.11.0,
navigation-compose 2.10.2, work-runtime-ktx 2.12.0, `net.java.dev.jna:jna:5.19.1@aar`,
kotlinx-coroutines-android 1.11.0. applicationId/namespace **`app.magnetar`**, ffi package `app.magnetar.ffi`.
`.so` built by `scripts/build-android.sh` (cargo-ndk `-t arm64-v8a -t x86_64 -P 26 -o android/app/src/main/jniLibs`),
Kotlin bindings generated from the **unstripped host** build (`strip=true` removes uniffi metadata).
Release: `isMinifyEnabled = false` (JNA + R8 untested). If a build dies with EIO check
`journalctl -k | grep csum` (laptop has shown btrfs csum errors in ~/.gradle).

### 8.2 App (Kotlin, Compose, no Google services)
- Screens: Search (query, filter sheet, sort), Detail (files, trackers, infohash, npub, liveness, trust
  explanation, comments; actions open/copy magnet, pin, hide, block publisher, download), Saved searches,
  Downloads, Settings (server URL, token, root npub, threshold, cache cap, relays, ntfy), Status (stats, last crawl/sync).
- WorkManager `CrawlSyncWorker` periodic 1 h, constraints **unmetered + charging**: crawl_once → sync_now →
  cleanup → poll_saved_searches → local notification for new hits.
- Foreground service (`dataSync`) for phone downloads.
- Cleartext allowed only for `127.0.0.1`/`localhost` (network_security_config); Tailscale/HTTPS otherwise.

### 8.3 Downloads
**Spike result (android.md §4): librqbit 9.0.1 (`default-features=false, features=["rust-tls"]`)
compiles for arm64-v8a + x86_64 and downloaded a real torrent on the API 37 x86_64 emulator; arm64 runtime
unverified.** So phone downloads run in the ffi crate (cargo feature `downloads`, on by default) with
explicit app-private paths for session + DHT persistence (never `ProjectDirs`), output in
`getExternalFilesDir(DIRECTORY_DOWNLOADS)/magnetar`. Hosted by a `dataSync` foreground service started only
from a user tap; `onTimeout` (Android 15 6 h cap) pauses all, persists, notifies, `stopSelf()`.
If the engine fails to start, `download_start(on_server=false)` returns `FfiError::Invalid{msg}` and the
app falls back to an `Intent(ACTION_VIEW, magnet)` (catch `ActivityNotFoundException` → copy magnet).
Server downloads use the same librqbit features in `magnetar-server` (output `MAGNETAR_DOWNLOAD_DIR`).
`core::downloads` holds the shared `Downloader` wrapper (librqbit Session → `DownloadInfo`), behind
core feature `downloads` so core stays light when it is off.

---

## 9. Server process (`magnetar-server`)

CLI: `magnetar-server serve | migrate | crawl-once [--seconds N] | selftest | --version`.
Env/config: `DATABASE_URL`, `MAGNETAR_BIND` (default `127.0.0.1:8787`), `MAGNETAR_API_TOKEN` (required for
serve), `MAGNETAR_ROOT_NPUB`, `MAGNETAR_NTFY_URL`, `MAGNETAR_NTFY_TOPIC`, `MAGNETAR_DOWNLOAD_DIR`,
`MAGNETAR_CRAWL_INTERVAL_SECS` (default 900, 0 = continuous, D32), `MAGNETAR_CRAWL_MAX_RELAYS` (30),
`MAGNETAR_CRAWL_EXPLORE_RELAYS` (10), `MAGNETAR_SEED_RELAYS` (comma), `MAGNETAR_NIP66` (default true),
`MAGNETAR_NIP66_MONITORS` (comma), `MAGNETAR_NIP66_TRUSTED` (comma), `MAGNETAR_DB_POOL` (10, D37), `RUST_LOG`.
`serve` runs: API + relay endpoint, crawler loop, liveness loop (batch of due infohashes every 5 min),
trust recompute (on graph_version change, else every 6 h), saved-search notifier (after a crawl, at most every 15 min), NIP-66
relay discovery (startup, then every 6 h; D35).
sqlx pool ≤ `MAGNETAR_DB_POOL` (default 10; was a fixed 5 for Termux, D37). `selftest` checks DB, DNS, TLS (fetch NIP-11 of a seed relay), UDP.

Seed relays (nostr-nips.md §4.5): `wss://nostr.oxtr.dev, wss://relay.nostr.net, wss://nos.lol,
wss://relay.primal.net, wss://relay.damus.io, wss://relay.snort.social, wss://nostr.mom, wss://nostr21.com,
wss://nostr.wine, wss://nostr.land, wss://offchain.pub, wss://nostr.bitcoiner.social`.
Indexers for kinds 0/3/10000/10002: `wss://purplepag.es, wss://user.kindpag.es, wss://indexer.coracle.social`.
Phone crawl is bounded: max 8 relays, `max_seconds` budget, forward cursors first, backfill afterwards.

---

## 10. Testing
- `core`: unit tests on every fixture line in `docs/research/fixtures/` (parse, verify), deviation table
  cases, magnet, pow, bencode, UDP/HTTP scrape against in-process fake trackers, Bloom256 test vector
  (estimate ≈ 1224.93), trust formula table, negentropy sync against an in-process relay.
- `server`: integration tests against Postgres (`MAGNETAR_TEST_DATABASE_URL`): ingest fixtures, search
  filters/sorts, relay REQ, NIP-77 with nostr-sdk `Client::sync` as oracle, batch POST, auth.
- `ffi`: SqliteStore search parity with PgStore on the fixture set; cleanup never deletes unsynced rows;
  sync to a live in-process server.
- Network tests against real relays/trackers are `#[ignore]` (run with `--ignored`).
- `scripts/test.sh` = fmt check + clippy `-D warnings` + tests (with DB) + Android assembleDebug.
