# Rust crate pins & APIs (verified 2026-10-25)

Scratch project that **compiles, runs, passes clippy, and cross-compiles for aarch64/x86_64-linux-android**:
`/tmp/claude-1000/magnetar-research/rust-crates/` (workspace: `probe` = server-side stack, `ffi` = uniffi+rusqlite cdylib, `uniffi-bindgen` = bindgen binary).
Sources: crates.io API (`/api/v1/crates/<name>[/<ver>[/dependencies]]`), crate sources in `~/.cargo/registry/src/*/<crate>-<ver>/` (read directly), CHANGELOGs shipped in the crates, real runs against relays and Postgres 17.11 (docker).

## 0. Toolchain gotcha (IMPORTANT)

- `PATH` has `/usr/bin/cargo` + `/usr/bin/rustc` = **Arch system rust 1.99.1** first. It has **no android std**.
- rustup toolchain = `~/.cargo/bin` -> **stable 1.99.0**, with targets `aarch64-linux-android`, `x86_64-linux-android` installed.
- Every Android build must run with `PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH`. Otherwise it fails with `E0463 can't find crate for core`.
- `cargo-ndk 4.1.2` (in `~/.local/bin`). `ANDROID_NDK_HOME=~/Android/Sdk/ndk/29.0.14206865`.
- NDK r29 output has 16 KB-aligned LOAD segments (`readelf -lW` shows `0x4000`), which Android 15+ requires. Verified.

## 1. Pinned versions (crates.io max_stable on 2026-10-25)

| crate | pin | MSRV | notes |
|---|---|---|---|
| nostr | 0.45.5 | 1.85 | repo moved to `github.com/nostrdevkit/nostr`. Big API break in 0.45 (see §2) |
| nostr-sdk | 0.45.4 | 1.85 | default features `ring` + `rustls-tls-webpki-roots`. Depends on `negentropy ^0.5`, `nostr-database ^0.45`, `nostr-gossip` |
| negentropy | 0.5.2 | 1.51 | the NIP-77 engine. Use it directly for the server relay endpoint |
| nostr-database | 0.45.2 | | the `NostrDatabase` trait (pulled in by nostr-sdk) |
| nostr-relay-builder | 0.44.1 | | **DO NOT USE**: it depends on `nostr ^0.44` and so cannot be mixed with 0.45. Write our own NIP-01/77 endpoint on axum ws |
| nostr-sqlite | 0.45.2 | | **avoid**: it uses `rusqlite ^0.38` and clashes with our rusqlite pin (links conflict) |
| sqlx | 0.9.0 | **1.94** | MSRV 1.94 is the floor for the whole workspace (and for any Termux `rustc`, UNVERIFIED what Termux ships) |
| axum | 0.8.9 | 1.80 | features `ws`, `macros` |
| tokio | 1.54.1 | 1.85 | |
| tower-http | 0.7.1 | 1.65 | reqwest 0.13 still pulls tower-http 0.6.11, so two copies exist. Harmless |
| rusqlite | **0.39.0** (not 0.41.0) | n/a | see §5 links conflict |
| libsqlite3-sys | 0.37.0 (transitive) | | bundles SQLite **3.51.3**, `ENABLE_FTS5=1` (verified at runtime) |
| uniffi | 0.32.2 | (none declared) | features `tokio` (async exports). The `cli` feature only on the bindgen bin |
| tracing | 0.1.44 | | |
| tracing-subscriber | 0.3.23 | 1.65 | features `env-filter`, `fmt` |
| serde | 1.0.229 | | `derive` |
| serde_json | 1.0.154 | | |
| thiserror | 2.0.21 | 1.77 | (1.0.69 is also pulled transitively, fine) |
| reqwest | 0.13.5 | 1.85 | **default TLS is now rustls+aws-lc-rs + rustls-platform-verifier** (see §7) |
| rustls | 0.23.46 | | `ring` provider |
| webpki-roots | 1.0.11 | | |
| librqbit | 9.0.1 | n/a | (downloads, other agent). Features: `default-tls`, `rust-tls`, `http-api`, … |
| sqlx-cli | 0.9.0 | | only needed if we use `query!` macros (we don't, §4) |

Kotlin side: JNA `net.java.dev.jna:jna:5.19.1@aar`, `kotlinx-coroutines-android:1.11.0` (Maven Central `maven-metadata.xml` latest), and `androidx.annotation` (needed when `android = true` in uniffi.toml).

## 2. Working workspace Cargo.toml (builds, runs, clippy clean, cross-compiles)

```toml
[workspace]
resolver = "3"
members = ["probe", "ffi", "uniffi-bindgen"]

[workspace.package]
edition = "2024"
rust-version = "1.94"

[workspace.dependencies]
nostr = { version = "0.45.5", default-features = false, features = ["std"] }
nostr-sdk = { version = "0.45.4" }            # defaults: ring + rustls webpki roots
negentropy = "0.5.2"
sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "tls-rustls-ring-webpki", "postgres", "macros", "migrate", "json", "chrono"] }
axum = { version = "0.8.9", features = ["ws", "macros"] }
tokio = { version = "1.54.1", features = ["full"] }
tower-http = { version = "0.7.1", features = ["trace", "cors", "timeout", "limit"] }
rusqlite = { version = "0.39", features = ["bundled"] }
uniffi = { version = "0.32.2", features = ["tokio"] }
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter", "fmt"] }
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.154"
thiserror = "2.0.21"
reqwest = { version = "0.13.5", default-features = false, features = ["rustls-no-provider", "json", "http2"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
webpki-roots = "1.0.11"
```

ffi crate:
```toml
[lib]
crate-type = ["cdylib", "lib"]   # "lib" so tests and the bindgen can link it
name = "magnetar_ffi"
[dependencies]
uniffi.workspace = true
rusqlite.workspace = true
thiserror.workspace = true
tokio.workspace = true
```
uniffi-bindgen crate (workspace binary):
```toml
[package]
name = "uniffi-bindgen"
[dependencies]
uniffi = { workspace = true, features = ["cli"] }
```
```rust
// uniffi-bindgen/src/main.rs
fn main() { uniffi::uniffi_bindgen_main() }
```
Result: no `aws-lc-*`, no `openssl-sys`, no `native-tls` in Cargo.lock, and only `ring` as crypto backend. That makes the cross-compile painless (no cmake or NASM needed). Verified: `cargo ndk -t arm64-v8a -P 26 build -p probe --release` gives a 17 MB PIE binary with `interpreter /system/bin/linker64`, which is usable in Termux (runtime on the device is UNVERIFIED).

## 3. nostr 0.45.5 API (read from source)

### Breaking changes in 0.45.0 (from CHANGELOG), relevant to us
- `JsonUtil` trait removed. `from_json`/`as_json`/`try_as_json` are now **inherent methods** (macro `impl_json_methods!`) on `Event`, `UnsignedEvent`, `Filter`, `ClientMessage`, `RelayMessage`, `RelayInformationDocument`. `Event::from_json(impl AsRef<[u8]>)` is plain `serde_json::from_slice`. **It does NOT verify**, so call `.verify()`.
- `EventBuilder::sign_with_keys`, `EventBuilder::pow` and `TagStandard`/`TagKind` were all removed. Tag access is string based: `tag.kind() -> &str`, `tag.content() -> Option<&str>` (index 1), `tag.as_slice() -> &[String]`.
- Signing is now through traits: `FinalizeEvent<S>::finalize(self, &signer) -> Result<Event,_>` (sync), `FinalizeEventAsync`, `FinalizeUnsignedEvent::finalize_unsigned(self, PublicKey) -> UnsignedEvent`. `Keys` implements the sign traits. Import via `nostr::prelude::*`.
- PoW: `UnsignedEvent::mine(self, &impl PowAdapter, NonZeroU8) -> Result<UnsignedEvent,_>` and `mine_async`. Adapters: `nostr::nips::nip13::SingleThreadPow`, `MultiThreadPow` (feature `pow-multi-thread`). NIP-13 helpers: `nip13::get_leading_zero_bits(h)`, `Event::check_pow(u8) -> bool` (= `id.check_pow`), `Tag::pow(nonce: u128, difficulty: u8)`.
- Error type path: `nostr::error::Error` (`nostr::event::Error` is private).

### Types
```rust
#[non_exhaustive] pub struct Event { pub id: EventId, pub pubkey: PublicKey, pub created_at: Timestamp,
                                     pub kind: Kind, pub tags: Tags, pub content: String, /* pub sig */ }
// Tags: Deref<Target=Vec<Tag>>, so .iter(), indexing. Also .hashtags(), .public_keys(), .identifier(), .expiration()
Event::verify(&self) -> Result<(), nostr::error::Error>   // id + schnorr sig
Event::verify_id(&self) -> bool;  Event::verify_signature(&self) -> bool
Event::is_expired(&self) -> bool  // NIP-40
```
- `Kind::Torrent` = 2003, `Kind::TorrentComment` = 2004, `Kind::ContactList` = 3, `Kind::RelayList` = 10002. **`Kind::Custom(2003) == Kind::Torrent`** (the custom PartialEq goes through u16. Verified with `assert_eq!` at runtime). `Kind::from_u16`/`as_u16` are const.
- `nostr::nips::nip35::Torrent { title, description, info_hash: Sha1Hash, files: Vec<TorrentFile{name,size}>, trackers: Vec<Url>, categories, hashtags }` is **builder only** (`IntoEventBuilder`). It emits tags `["title",..]`, `["x",hex]`, `["file",name,size]`, `["tracker",url]`, `["i","tcat:<cat>"]`, `["t",..]`. **There is no parser**, so core must write its own NIP-35 parser (useful as the reference for the tag shapes we generate in tests).
- Bech32: `pubkey.to_bech32()?` gives `npub1…`, and `PublicKey::parse(&str)` accepts hex, npub, nprofile, and `nostr:` URIs.
- Timestamps: `Timestamp::now()`, `Timestamp::from(u64)`, `.as_secs()` (`as_u64` was removed in 0.45).

### Filter (builder, consumes self)
`Filter::new().kind(k).kinds([..]).author(pk).authors([..]).since(ts).until(ts).limit(usize).ids([..]).hashtag(s).custom_tag(SingleLetterTag, v).search(s)`.
`Filter::match_event(&self, &Event, MatchEventOptions) -> bool` (useful for the relay endpoint's REQ matching).
Wire output verified: `{"kinds":[2003,2004],"since":1700000000,"until":1792918484,"limit":500}`.

### Verified snippet (compiled and ran)
```rust
use nostr::prelude::*; use nostr::nips::nip13::SingleThreadPow; use std::num::NonZeroU8;
let keys = Keys::generate();
let ev: Event = EventBuilder::new(Kind::Torrent, "desc")
    .tag(Tag::custom("x", ["0123456789abcdef0123456789abcdef01234567"]))
    .tag(Tag::custom("file", ["a.mkv", "1234"]))
    .tag(Tag::custom("i", ["tcat:video,movie"]))
    .finalize_unsigned(keys.public_key())
    .mine(&SingleThreadPow, NonZeroU8::new(8).unwrap())?
    .finalize(&keys)?;
ev.verify()?; assert!(ev.check_pow(8));
let back = Event::from_json(ev.as_json())?;           // tampered content gives verify() Err (tested)
let x = back.tags.iter().find(|t| t.kind() == "x").and_then(|t| t.content());
```

### NIP-77 message types (in nostr, for our own relay endpoint)
`ClientMessage::NegOpen { subscription_id: Cow<SubscriptionId>, filter: Cow<Filter>, initial_message: Cow<str> }`, `NegMsg { subscription_id, message }`, `NegClose { subscription_id }`. `RelayMessage::NegMsg { subscription_id, message }`, `NegErr { subscription_id, message }`. In 0.45, `NegOpen` no longer has `id_size`. `RelayMessage::ok(event_id, bool, msg)`. All are parsed and serialized with `ClientMessage::from_json(&str)` / `.as_json()` (verified in the axum ws handler).
negentropy 0.5.2: `Storage` (`insert(created_at:u64, Id)`, `seal()`), `Negentropy::owned(storage, frame_size_limit)` / `borrowed`, `initiate() -> Vec<u8>`, `reconcile(&[u8]) -> Vec<u8>`, `reconcile_with_ids(query, &mut have, &mut need)`. Messages are hex encoded on the wire (NIP-77, details in the negentropy research note).

## 4. nostr-sdk 0.45.4 Client (builder-future API; `RelayPoolNotification` is gone)

- `Client::default()` / `Client::builder().verify_subscriptions(true).database(db).build()`. The default database is `MemoryEventsTracker` (it tracks seen ids and **does not store events**).
- `client.add_relay("wss://…").await?` (returns an `AddRelay` builder: `.capabilities(..)`, `.connect_timeout(..)`, `.verify_subscriptions(true)`, `.and_connect()`), then `client.connect().await`. A failing relay only logs an `ERROR`; the other relays keep going (damus.io returned 503 during the test).
- **One-shot fetch:** `client.fetch_events(filter).timeout(Duration).policy(ReqExitPolicy::ExitOnEOSE).max_events(n).await? -> Events` (an iterable of `Event`).
- **Stream:** `let mut s = client.stream_events(filter).timeout(d).policy(p).await?; while let Some((relay_url, res)) = s.next().await { let ev = res?; }` uses `nostr_sdk::prelude::StreamExt`.
- **Long-lived:** `let out = client.subscribe(filter).with_id(id).close_on(SubscribeAutoCloseOptions).await?;` plus `client.notifications()` → `Stream<Item = ClientNotification>` where `ClientNotification::{ Event{relay_url, subscription_id, event: Box<Event>}, Message{relay_url, message}, Shutdown }`. Subscribe to notifications **before** calling subscribe.
- Targets: `F: Into<ReqTarget>`. A `Filter` means auto (all READ relays, plus NIP-65 relays if gossip is enabled). `ReqTarget::single(url, filters)`, or a `HashMap<&str, Vec<Filter>>` for per-relay filters (this is how we do per-relay cursors).
- `ReqExitPolicy::{ExitOnEOSE (default), WaitForEvents(u16), WaitForEventsAfterEOSE(u16), WaitDurationAfterEOSE(Duration)}`.
- Semantics (from the docs and source): events are **deduplicated across relays by id**, and **signatures are validated** (`verify_and_cache`). Filter matching is enforced only with `verify_subscriptions(true)`. The server must still re-verify on ingest from phone uploads (that path doesn't go through nostr-sdk).
- **Pagination (verified live):** loop `until = oldest.created_at - 1` with `limit(100)`. `limit` is **per relay** and the merged result is deduped. With 3 relays, pages of 109/111/114 came back from 2 live relays. Stop when the page is empty. Per-relay cursors are more accurate (the `HashMap` target). Live data: kind 2003 is busy (≈330–450 events in the last ~1 day across nos.lol + relay.primal.net).
- Sync (NIP-77) built in: `client.sync(filter).with([url]).opts(SyncOptions::new().direction(SyncDirection::Up|Down|Both).initial_timeout(d).idle_timeout(d).dry_run().progress(tx)).await? -> SyncSummary { local, remote: HashMap<EventId,HashSet<RelayUrl>>, sent, received, send_failures: HashMap<RelayUrl,HashMap<EventId,String>> }`. It reads local items from `client.database()`, so using it phone→server needs a `NostrDatabase` impl over our SQLite (methods: `backend, features, save_event, check_id, event_by_id, count, query, negentropy_items (has default), delete, wipe`, all returning `Pin<Box<dyn Future>>`). `SyncSummary.sent` minus `send_failures` = the ids to mark synced (UNVERIFIED end to end; the alternative is a hand-rolled negentropy client with the `negentropy` crate). `sync_with` is deprecated.
- `client.shutdown().await`.

## 5. rusqlite / SQLite / FTS5

- **Links conflict:** `sqlx 0.9.0` has an optional dep `sqlx-sqlite = 0.9.0 → libsqlite3-sys >=0.30.1,<0.38`. Cargo checks `links = "sqlite3"` across the **whole lockfile, including unactivated optional deps**, so `rusqlite 0.41` (libsqlite3-sys 0.39) and `0.40` (0.38) **fail to resolve** in the same workspace as sqlx 0.9. → **Pin `rusqlite = "0.39"` (libsqlite3-sys 0.37.0, SQLite 3.51.3).** Alternative: a separate workspace for ffi (rejected: two lockfiles).
- `features = ["bundled"]` → `ENABLE_FTS5` compiled in (`SELECT sqlite_compileoption_used('ENABLE_FTS5')` = 1, verified host build). Cross-compiles with cargo-ndk with no extra setup (cc from NDK).
- Working FTS5 schema (external content + triggers), tested:
```sql
CREATE TABLE torrents(rowid INTEGER PRIMARY KEY, event_id TEXT UNIQUE NOT NULL, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', files_text TEXT NOT NULL DEFAULT '');
CREATE VIRTUAL TABLE torrents_fts USING fts5(title, files_text, description,
   content='torrents', content_rowid='rowid',
   tokenize="unicode61 remove_diacritics 2 separators '._-[]()'");
CREATE TRIGGER torrents_ai AFTER INSERT ON torrents BEGIN
  INSERT INTO torrents_fts(rowid,title,files_text,description) VALUES(new.rowid,new.title,new.files_text,new.description); END;
CREATE TRIGGER torrents_ad AFTER DELETE ON torrents BEGIN
  INSERT INTO torrents_fts(torrents_fts,rowid,title,files_text,description) VALUES('delete',old.rowid,old.title,old.files_text,old.description); END;
-- also need an AFTER UPDATE trigger (delete + insert)
SELECT t.event_id, bm25(torrents_fts, 10.0, 3.0, 1.0) AS r
FROM torrents_fts JOIN torrents t ON t.rowid = torrents_fts.rowid
WHERE torrents_fts MATCH ?1 ORDER BY r LIMIT 50;   -- bm25: lower = better
```
  `separators '._-[]()'` makes `Big.Buck.Bunny.2008.1080p.x264-GRP` match `bunny 1080p`. Prefix `bun*`, phrase `"big buck"`, `title:ubuntu NOT bunny` all work. **Raw user input can raise a syntax error** (`"unterminated` gives `unterminated string`), so sanitize: quote each token as `"tok"` (double any `"`) and add `*` for prefix.
- Use `PRAGMA journal_mode=WAL`. Keep the `Connection` in a `Mutex` inside the uniffi Object (`rusqlite::Connection` is `Send`, not `Sync`).

## 6. uniffi 0.32.2 (proc-macro, library mode). Verified to build, and Kotlin was generated

```rust
uniffi::setup_scaffolding!();                       // lib.rs, no UDL, no build.rs
#[derive(Debug, thiserror::Error, uniffi::Error)] pub enum FfiError { #[error("sqlite: {msg}")] Sqlite { msg: String } }
#[derive(uniffi::Record)] pub struct SearchHit { pub event_id: String, pub rank: f64, pub size_bytes: Option<u64> }
#[derive(uniffi::Enum)] pub enum SortBy { Relevance, Date, Size, Seeders }
#[derive(uniffi::Object)] pub struct Store { conn: Mutex<rusqlite::Connection> }
#[uniffi::export] impl Store {
    #[uniffi::constructor] pub fn open(path: String) -> Result<Arc<Self>, FfiError> { .. }
    pub fn search(&self, q: String, sort: SortBy) -> Result<Vec<SearchHit>, FfiError> { .. } }
#[uniffi::export(async_runtime = "tokio")] pub async fn async_ping(ms: u64) -> String { .. }  // needs uniffi feature "tokio"
#[uniffi::export(callback_interface)] pub trait ProgressSink: Send + Sync { fn on_progress(&self, done: u64, total: u64); }
#[uniffi::export(with_foreign)] pub trait Logger: Send + Sync { fn log(&self, line: String); }
```
Generate (library mode is auto-detected; `--library` is now a deprecated no-op):
```sh
cargo build -p magnetar-ffi                     # host .so for bindgen metadata
cargo run -p uniffi-bindgen -- generate target/debug/libmagnetar_ffi.so --language kotlin --out-dir android/app/src/main/java --no-format
PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH ANDROID_NDK_HOME=~/Android/Sdk/ndk/29.0.14206865 \
  cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/app/src/main/jniLibs build -p magnetar-ffi --release
```
`ffi/uniffi.toml`:
```toml
[bindings.kotlin]
package_name = "app.magnetar.ffi"
cdylib_name = "magnetar_ffi"
android = true            # Android cleaner: SystemCleaner on API>=34 (UPSIDE_DOWN_CAKE), else JNA cleaner. Needs androidx.annotation
# generate_immutable_records = true   # optional: val instead of var
```
Generated Kotlin (checked): `package app.magnetar.ffi`; `open class Store : Disposable, AutoCloseable, StoreInterface` with `companion object { @Throws(FfiException::class) fun open(path: String): Store }`; `data class SearchHit(var eventId: String, var rank: Double, var sizeBytes: ULong?)` (snake → camel, u64 → `ULong`); `sealed class FfiException : Exception() { class Sqlite(val msg: String) }` (Rust `FfiError` → Kotlin `FfiException`); `enum class SortBy`; `suspend fun asyncPing(ms: ULong): String`; `interface ProgressSink`, `interface Logger`. Needs JNA (`Native.register`) + kotlinx-coroutines. The library name is resolved via `findLibraryName("magnetar_ffi")` (override with the `uniffi.component.<name>.libraryOverride` system property). Without ktlint the formatter prints a warning, so pass `--no-format`. Gotcha (UNVERIFIED this run, known uniffi issue): don't name an error variant field `message`, because it clashes with `Exception.message`. Use `msg`/`reason`.
Output: `libprobe_ffi.so` arm64 2.7 MB / x86_64 2.6 MB release (unstripped), `for Android 26, built by NDK r29`.

## 7. reqwest 0.13.5 TLS: Android gotcha

- In 0.13, `default-tls` = `rustls` = **aws-lc-rs + rustls-platform-verifier**. On `target_os = "android"`, the platform verifier needs JNI init (`rustls_platform_verifier::android::init_with_env`). That is impossible in a Termux CLI and awkward in the app, so building a Client would fail or panic there. Also with extra roots on Android, reqwest returns the error `"rustls-platform-verifier could not load extra certs"` (source: `async_impl/client.rs` ~L758–780).
- Fix used (verified: HTTPS 200 against crates.io): features `["rustls-no-provider","json","http2"]` plus our own rustls config:
```rust
fn http_client() -> reqwest::Result<reqwest::Client> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions().expect("tls")
        .with_root_certificates(roots).with_no_client_auth();
    reqwest::Client::builder().user_agent("magnetar/0.1").timeout(Duration::from_secs(10))
        .tls_backend_preconfigured(tls).build()
}
```
  `tls_certs_only(certs)` would also bypass the platform verifier. Also call `rustls::crypto::ring::default_provider().install_default()` once at startup (harmless, and it covers other rustls users).
- The renames in 0.13: `use_rustls_tls` → `tls_backend_rustls`, `use_preconfigured_tls` → `tls_backend_preconfigured` (the old names still exist, deprecated).

## 8. sqlx 0.9.0 (Postgres)

- Features: `runtime-tokio`, `tls-rustls-ring-webpki` (or `tls-none`, since we only connect to 127.0.0.1), `postgres`, `macros`, `migrate`, `json`, `chrono`. The old combined features (`runtime-tokio-rustls`) were **deleted** in 0.9.
- **Breaking:** `sqlx::query/query_as/query_scalar` take `impl SqlSafeStr`, which is implemented only for `&'static str`, `AssertSqlSafe<String|&str|..>`, and `SqlStr`. A dynamic SQL string must be wrapped: `sqlx::query(AssertSqlSafe(sql))`. `QueryBuilder` is still available for dynamic filters (preferred).
- `sqlx::migrate!("./migrations")` is checked at compile time against the directory only (**no DB needed at build time**, verified). Run it with `.run(&pool).await?`. Migration files are named `<VERSION>_<desc>.sql`, e.g. `20261025000000_init.sql`.
- Decision: use runtime-checked `query_as::<_, T>()` + `#[derive(sqlx::FromRow)]`, not `query!`. That way no `DATABASE_URL` or `.sqlx` offline data is needed to build in Termux.
- Pool: `PgPoolOptions::new().max_connections(5).acquire_timeout(Duration::from_secs(5)).connect(&url).await?`.
- Termux socket: `postgres://user@/magnetar?host=$PREFIX/var/run/postgresql`, or TCP `postgres://user:pw@127.0.0.1:5432/magnetar` (Termux specifics are in another note).

## 9. Postgres full text + trigram (verified on PostgreSQL 17.11)

```sql
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE TABLE torrents (
  event_id    text PRIMARY KEY,
  title       text NOT NULL DEFAULT '',
  description text NOT NULL DEFAULT '',
  files_text  text NOT NULL DEFAULT '',
  search_tsv  tsvector GENERATED ALWAYS AS (
      setweight(to_tsvector('simple', regexp_replace(coalesce(title,''),      '[._\-\[\]()+]+', ' ', 'g')), 'A') ||
      setweight(to_tsvector('simple', regexp_replace(coalesce(files_text,''), '[._\-\[\]()+/]+', ' ', 'g')), 'B') ||
      setweight(to_tsvector('english', coalesce(description,'')), 'C')
  ) STORED
);
CREATE INDEX torrents_search_tsv_idx ON torrents USING GIN (search_tsv);
CREATE INDEX torrents_title_trgm_idx ON torrents USING GIN (lower(title) gin_trgm_ops);
```
- A generated column needs immutable expressions. `to_tsvector(<regconfig literal>, …)` and `regexp_replace` qualify (accepted by PG17). **A one-argument `to_tsvector(text)` is NOT immutable**, so always pass the config.
- **Gotcha:** without the `regexp_replace`, `to_tsvector('simple','Big.Buck.Bunny.2008.1080p.x264-GRP')` gives `'2008.1080' 'big.buck.bunny' 'p.x264-grp'`, so release names don't match their words. With it you get `big buck bunny 2008 1080p x264 grp`. Verified.
- Search query (verified, binds `$1` text, `$2` int8):
```sql
SELECT event_id, title, ts_rank_cd(search_tsv, q, 32)::float4 AS rank
FROM torrents, websearch_to_tsquery('simple', $1) AS q
WHERE search_tsv @@ q
ORDER BY rank DESC
LIMIT $2;
```
  `websearch_to_tsquery('simple','"big buck" -ubuntu or linux')` → `'big' <-> 'buck' & !'ubuntu' | 'linux'`. It never errors on user input. **It does not support prefix** (`bun:*` → `'bun'`). For type-ahead build `to_tsquery('simple', 'bun:* & 108:*')` from sanitized tokens (verified to work). Use `'english'` only on the description (stemming: `rabbit` matched `rabbits`). Rank normalization 32 = `rank/(rank+1)` gives 0..1.
- Fuzzy / typo fallback with the trigram GIN (index use verified via `EXPLAIN` → `Bitmap Index Scan on t2_trgm, Index Cond: (lower(title) %> 'bunni')`):
```sql
SELECT event_id, title, word_similarity($1, lower(title)) AS sim
FROM torrents WHERE $1 <% lower(title) ORDER BY sim DESC LIMIT 20;
```
  Use `<%` / `word_similarity` (threshold `pg_trgm.word_similarity_threshold` = 0.6), **not** `%`/`similarity` (0.3 on the whole title, so `ubunto` vs `Ubuntu 24.04 desktop amd64` gives 0.17 and misses). The query side must also be `lower()` to match the expression index. Combined: `WHERE search_tsv @@ q OR $1 <% lower(title)` ordered by `ts_rank_cd DESC, word_similarity DESC` (works). On small tables the planner picks a seq scan (expected).

## 10. axum 0.8.9 / tower-http 0.7.1 (compiled and served)

- Path params use `/{id}` syntax (0.8; `/:id` is gone). `axum::serve(tokio::net::TcpListener, Router)`. Bind `127.0.0.1` only.
- WS: `WebSocketUpgrade` → `ws.on_upgrade(|socket| ..)`. `Message::Text(Utf8Bytes)`, so build it with `Message::Text(json.into())` and read with `t.as_str()`.
- Layers that compiled: `TraceLayer::new_for_http()`, `CorsLayer::permissive()`, `TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, d)` (`TimeoutLayer::new` is deprecated since 0.6.7, so clippy would warn), `RequestBodyLimitLayer::new(8<<20)`.
- `Router::new().route(..).with_state(state)`. The handler state is `State<T: Clone>`.

## 11. tracing

`tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();` → set `RUST_LOG=info,magnetar=debug`. nostr-sdk logs relay connection failures at ERROR level (noisy), so suggest `nostr_sdk=warn` or `off` for relay churn.

## 12. Open / UNVERIFIED
- The aarch64 Termux binary was built but not executed on a device (no device attached). The `rustls-platform-verifier` bypass is reasoned from source, not tested on Android.
- `client.sync()` phone→server with a custom `NostrDatabase` was not exercised (it needs a relay endpoint that supports NIP-77).
- Termux's `rust` package version is unknown. It must be ≥ 1.94 for sqlx 0.9 (otherwise cross-compile on the PC as above).
