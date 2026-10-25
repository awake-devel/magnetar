# NIP-77 negentropy: research notes for Magnetar

Researched 2026-10-25. Sources: `nostr-protocol/nips` master `77.md` (raw), crates.io API, and
crate sources in `~/.cargo/registry/src/*/` (negentropy-0.5.2, nostr-0.45.5, nostr-sdk-0.45.4,
nostr-database-0.45.2, nostr-memory-0.45.2, nostr-relay-builder-0.45.0-alpha.3).
Everything below marked **VERIFIED** was compiled and run in
`/tmp/claude-1000/magnetar-research/negentropy/scratch` (axum relay + nostr-sdk client, all asserts
passed) or checked against live relays. Anything I did not run is marked **UNVERIFIED**.

---

## 1. Decision summary

| Question | Answer |
|---|---|
| Negentropy crate | `negentropy = "0.5.2"` (2026-10-15, MIT, MSRV 1.51, `no_std`-capable, pure Rust). **0.5.0, 0.5.1 and 0.4.3 are YANKED.** 0.5.2 adds a security fix: it rejects an IdList count larger than the IDs left in the message, so a huge count cannot force a big allocation. |
| Relay side (server) | **Write it ourselves** in the axum WS handler with `negentropy` 0.5.2, using the `nostr` 0.45.5 message types (`ClientMessage`/`RelayMessage`, `Event::verify`). It is about 150 LOC. **VERIFIED** end-to-end against nostr-sdk's `Client::sync`. |
| Client side (phone, ffi crate) | **Also write it ourselves** (initiator loop with `negentropy` + a WS client), because we need exact control of "mark synced only after OK" and of the SQLite source. Use `nostr-sdk` 0.45.4 `Client::sync` **only as a dev-dependency interop oracle** in server integration tests. |
| Reuse `nostr-relay-builder`? | **No.** It is stale: stable is 0.44.1 and 0.45 exists only as `-alpha.1..3`. It has been folded into `nostr-sdk` behind the feature `local-relay` (`nostr_sdk::local_relay::LocalRelay`). Embedding it needs a full `NostrDatabase` impl for Postgres (full NIP-01 `query()` returning `BTreeSet<Event>`). It does not mount natively in axum (it needs a raw hyper upgrade). It also hard-codes a 60 000-byte frame limit, and its default `max_negentropy_items` is 50 000, which is already exceeded by real kind-2003 data (see §6). |
| Frame size limit | Use `60_000` on both sides, the same as nostr-sdk (`NEGENTROPY_FRAME_SIZE_LIMIT`). That is about 120 KB of hex inside JSON. Allowed values are `0` (unlimited) or `>= 4096`. Otherwise `new()` returns `Err(FrameSizeLimitTooSmall)`. |

---

## 2. NIP-77 wire format (from `77.md`, VERIFIED against crate + live relays)

All payloads are hex strings of binary negentropy messages. NEG subscription IDs are a
**separate namespace from REQ subscription IDs**. A `NEG-OPEN` on an already-open ID first closes the old one.

```
client -> relay  ["NEG-OPEN",  <subid>, <NIP-01 filter object>, <initialMessage hex>]
relay  -> client ["NEG-MSG",   <subid>, <message hex>]          (bidirectional, alternating)
client -> relay  ["NEG-MSG",   <subid>, <message hex>]
client -> relay  ["NEG-CLOSE", <subid>]
relay  -> client ["NEG-ERR",   <subid>, "<prefix>: <human text>"]   (subscription is then closed)
```

- Suggested NEG-ERR prefixes are `blocked` (query too big or too old) and `closed` (the relay timed out
  idle state). With `blocked`, the max record count *may* be sent as a 4th element.
- The NIP has no NEG-MSG from the relay after the client is done. The client just stops, sends
  NEG-CLOSE, or disconnects.
- Negentropy only finds the IDs. The actual transfer is `EVENT` (upload, answered by `OK`) and `REQ`
  with `ids` (download), on the same socket and in parallel with NEG-MSGs.

### Binary protocol V1
- `Message := <version byte 0x61> <Range>*`. If a server gets an unknown version, it answers with a
  single byte, its highest supported version (`0x61`), and the client downgrades.
  - In the crate: a version outside `0x60..=0x6F` gives `Err(InvalidProtocolVersion)`. A version
    in range but `!= 0x61` makes the non-initiator reply `[0x61]` (**VERIFIED**: reply to `[0x62]` is `[0x61]`).
- Items are `(timestamp u64, id [u8;32])`, sorted by timestamp ascending, then by id bytes lexically.
  The timestamp `u64::MAX` is reserved as "infinity" and may not be used.
- `Varint` is base-128, **most-significant digit first**, with the high bit set on every byte except the last.
- `Range := <upperBound: Bound> <mode: Varint> <payload>`. Mode 0 is Skip (empty), mode 1 is
  Fingerprint (16 bytes), mode 2 is IdList (`<len Varint> <32-byte id>*`).
- `Bound := <encodedTimestamp Varint> <prefixLen Varint> <idPrefix bytes>`. The timestamp is
  encoded as `0` for infinity, otherwise `1 + (ts - previousTs)`. The delta resets per message.
  The prefix length is 0..=32 and missing trailing bytes count as 0.
- Fingerprint: add all IDs mod 2^256 as **little-endian** 256-bit ints, append the count as a Varint,
  hash with SHA-256, keep the first 16 bytes.
- **VERIFIED** byte example: the initiate message for an empty set is `61 00 00 02 00`
  (version, bound ts=infinity, prefix len 0, mode IdList, count 0), so hex `"6100000200"`.
  This is a handy probe payload to check whether a relay supports NIP-77.

---

## 3. `negentropy` 0.5.2 API (read from source, VERIFIED by compiling)

```rust
use negentropy::{Id, Negentropy, NegentropyStorageVector, NegentropyStorageBase, Storage, Error,
                 Bound, Item, PROTOCOL_VERSION /*0x61*/, ID_SIZE /*32*/, FINGERPRINT_SIZE /*16*/};

// storage
let mut s = NegentropyStorageVector::with_capacity(n);   // or ::new()
s.insert(created_at: u64, Id::from_byte_array([u8;32]))?; // Err(AlreadySealed) after seal
s.seal()?;            // sort_unstable + dedup; REQUIRED before use (else Err(NotSealed))
s.unseal()?;          // allows more inserts

// engine (T: NegentropyStorageBase)
Negentropy::owned(s, frame_size_limit)?     // Negentropy<'static, NegentropyStorageVector>: store per session
Negentropy::borrowed(&s, frame_size_limit)? // Negentropy<'a, _>
Negentropy::new(Storage::Owned(s) | Storage::Borrowed(&s), limit)?

// client / initiator
let init: Vec<u8> = neg.initiate()?;                          // once; 2nd call: Err(AlreadyBuiltInitialMessage)
let next: Option<Vec<u8>> = neg.reconcile_with_ids(&reply, &mut have_ids, &mut need_ids)?;
//   have_ids: Vec<Id>  = ids the CLIENT has and the server lacks  -> upload via EVENT
//   need_ids: Vec<Id>  = ids the SERVER has and the client lacks  -> REQ {"ids":[..]}
//   both vectors are APPENDED (not cleared) every round; the final round still appends
//   None => reconciliation complete (output would be just the version byte) -> send NEG-CLOSE
neg.set_initiator();   // to resume an initiator flow with a fresh instance

// server / responder
let reply: Vec<u8> = neg.reconcile(&query)?;   // Err(Initiator) if called on initiator
```

- `Id::from_slice(&[u8]) -> Result<Id, Error>`, `Id::to_bytes()`, `Id::as_bytes()`; `Id` derefs to `[u8;32]`.
- `Error` variants: `IdTooBig, InvalidIdSize, FrameSizeLimitTooSmall, NotSealed, AlreadySealed,
  AlreadyBuiltInitialMessage, Initiator, NonInitiator, UnexpectedMode(u64), ParseEndsPrematurely,
  ProtocolVersionNotFound, InvalidProtocolVersion, UnsupportedProtocolVersion, TryFromSlice, BadRange`.
  It implements `std::error::Error` and `Display`.
- The custom storage trait `NegentropyStorageBase` has these methods: `size()`, `get_item(i)`,
  `iterate(begin,end,cb)`, `find_lower_bound(first,last,&Bound)`, and an overridable
  `fingerprint(begin,end)`. It is **synchronous and index-based**, so it cannot sit directly on
  async sqlx. **Decision:** load `(created_at,id)` rows into a `NegentropyStorageVector` per NEG-OPEN
  (40 B/item: 1M items is about 40 MB, the current network about 2.3 MB).
- **The frame limit binds only the messages *this* side produces.** A peer may send bigger frames.
  Live strfry relays sent a 499 869-byte NEG-MSG (about 1 MB hex) to an initiator whose limit was 0.
  WS max-message settings must allow this. We set the server to 1 MiB because our phone client also uses 60 000.
- The **VERIFIED** multi-round run used 20k client items vs 25k server items, both with limit 60 000.
  It took 7 round trips, had a max frame of 59 905 B, and found have=5000 and need=10000 (exactly right).

---

## 4. nostr-sdk 0.45.4 client-side sync (VERIFIED)

```rust
use nostr_sdk::prelude::*;   // Client, SyncOptions, SyncDirection, SyncSummary, NostrDatabase
let client = Client::builder().database(nostr_memory::MemoryDatabase::unbounded()).build();
client.add_relay("ws://127.0.0.1:7777").await?;
client.connect().and_wait(Duration::from_secs(5)).await;     // wait_for_connection() is deprecated
let out: Output<SyncSummary> = client
    .sync(Filter::new().kinds([Kind::Torrent, Kind::TorrentComment]))   // Kind::Torrent=2003, TorrentComment=2004
    .with(["ws://127.0.0.1:7777"])
    .opts(SyncOptions::new().direction(SyncDirection::Both)   // Up | Down (default) | Both
          .initial_timeout(Duration::from_secs(10))          // default 10s; waits for first NEG-MSG/NEG-ERR
          .idle_timeout(Duration::from_secs(10))              // default 10s
          /* .dry_run() .progress(tx) */)
    .await?;
// out.value: SyncSummary { local: HashSet<EventId>  (we had, relay lacked),
//                          remote: HashMap<EventId, HashSet<RelayUrl>> (relay had, we lacked),
//                          sent: HashMap<EventId, HashSet<RelayUrl>>  (OK=true received),
//                          received: HashMap<..>, send_failures: HashMap<RelayUrl, HashMap<EventId,String>> }
// out.success: HashSet<RelayUrl>, out.failed: HashMap<RelayUrl, String>
```
- `Client::sync_with(urls, filter, &opts)` still exists but is **deprecated since 0.45.0**. Use `sync(f).with(urls).opts(o)`.
- The default `Client::new()` database is `MemoryEventsTracker`, which stores IDs only and has no events,
  so uploads find nothing. **You must pass a real `NostrDatabase`** (e.g. `nostr-memory` `MemoryDatabase::unbounded()`).
- Items come from `database.negentropy_items(filter)`. Uploads read `database.event_by_id(id)`.
  To use this on the phone we would have to implement all of `NostrDatabase` over our SQLite. That is another reason to hand-roll the phone client.
- Internals: frame limit 60 000. Upload uses an in-flight window: refill when in-flight is at most 50, up to 100 in flight.
  Down requests use batches of 100 IDs (`REQ {"ids":[...]}`). An `OK` with `true` goes into `sent`, and `false` goes into `send_failures`.
  An `OK true "duplicate:"` counts as sent, which is the semantics we want.
- **GOTCHA (VERIFIED live):** `client.sync(..).await` returns **`Ok`** even when the relay does not support
  NIP-77. Per-relay failures land in `out.failed` (`"negentropy not supported"`, `"timeout"`).
  Always check `out.failed`.
- Unsupported detection only recognises NOTICEs that contain "bad msg" plus "negentropy"/"NEG-"/"unknown cmd",
  as strfry sends them. Other relays (e.g. `"Unknown message type: NEG-OPEN"`) are found only by the initial timeout.

---

## 5. Server relay endpoint: verified sketch (axum 0.8.9, nostr 0.45.5, negentropy 0.5.2)

Full file: `/tmp/claude-1000/magnetar-research/negentropy/scratch/src/main.rs`. It ran green:
nostr-sdk `Client::sync(Both)` uploaded 100 and downloaded 100, the kind-1 note was excluded by the filter, and a 2nd sync was a no-op.

Cargo (verified resolution):
```toml
axum = { version = "0.8.9", features = ["ws"] }
negentropy = "0.5.2"
nostr = { version = "0.45.5", features = ["std", "rand", "os-rng"] } # rand/os-rng needed for Keys::generate
hex = "0.4.3"            # or faster-hex 1.0 (what nostr-sdk uses)
futures = "0.3"
tokio = { version = "1", features = ["full"] }
[dev-dependencies]
nostr-sdk = "0.45.4"     # interop oracle
nostr-memory = "0.45.2"
```

```rust
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostr::filter::MatchEventOptions;
use nostr::message::{ClientMessage, MachineReadablePrefix, RelayMessage};
use nostr::prelude::*;

const FRAME_SIZE_LIMIT: u64 = 60_000;
type NegSessions = HashMap<String, Negentropy<'static, NegentropyStorageVector>>; // per connection

async fn ws_handler(ws: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    ws.max_message_size(1 << 20).on_upgrade(move |s| conn(s, st))
}
async fn conn(socket: WebSocket, st: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut negs: NegSessions = HashMap::new();
    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(txt) = msg else { continue };
        let out = match ClientMessage::from_json(txt.as_str()) {
            Err(e) => vec![RelayMessage::notice(format!("error: bad msg: {e}"))],
            Ok(cm) => handle(cm, &st, &mut negs).await,
        };
        for m in out { if tx.send(Message::Text(m.as_json().into())).await.is_err() { return; } }
    }
}
fn neg_err(sub: &str, prefix: impl std::fmt::Display, why: &str) -> RelayMessage<'static> {
    RelayMessage::NegErr { subscription_id: Cow::Owned(SubscriptionId::new(sub)),
                           message: Cow::Owned(format!("{prefix}: {why}")) }
}
// in handle():
ClientMessage::NegOpen { subscription_id, filter, initial_message } => {
    let sid = subscription_id.to_string();
    negs.remove(&sid);                                   // re-open replaces
    let Ok(init) = hex::decode(initial_message.as_ref()) else {
        return vec![neg_err(&sid, MachineReadablePrefix::Invalid, "bad hex")] };
    let items = store.negentropy_items(&filter, MAX_NEG_ITEMS + 1).await?;   // Vec<(u64,[u8;32])>
    if items.len() > MAX_NEG_ITEMS {
        return vec![neg_err(&sid, MachineReadablePrefix::Blocked, "this query is too big")] }
    let mut sv = NegentropyStorageVector::with_capacity(items.len());
    for (ts, id) in items { sv.insert(ts, Id::from_byte_array(id)).unwrap(); }
    sv.seal().unwrap();
    let mut neg = Negentropy::owned(sv, FRAME_SIZE_LIMIT).unwrap();
    match neg.reconcile(&init) {
        Ok(reply) => { negs.insert(sid.clone(), neg);
            vec![RelayMessage::NegMsg { subscription_id: Cow::Owned(SubscriptionId::new(sid)),
                                        message: Cow::Owned(hex::encode(reply)) }] }
        Err(e) => vec![neg_err(&sid, MachineReadablePrefix::Invalid, &e.to_string())],
    }
}
ClientMessage::NegMsg { subscription_id, message } => { /* get_mut session (missing -> neg_err "closed"),
    hex-decode + neg.reconcile(&buf) -> NEG-MSG; on any error remove session + NEG-ERR invalid */ }
ClientMessage::NegClose { subscription_id } => { negs.remove(subscription_id.as_str()); vec![] }
ClientMessage::Event(ev) => {
    let ev = ev.into_owned();
    if let Err(e) = ev.verify() { return vec![RelayMessage::ok(ev.id, false, format!("invalid: {e}"))] }
    // + NIP-35 validation, then upsert; duplicate => OK true "duplicate: ..."
    vec![RelayMessage::ok(ev.id, true, "")]
}
ClientMessage::Req { subscription_id, filters } => { /* EVENT* then RelayMessage::eose(sid) */ }
```
API facts checked in nostr 0.45.5:
- `ClientMessage<'a>` variants are `Event(Cow<Event>)`, `Req{subscription_id, filters: Vec<Cow<Filter>>}`,
  `Count{..}`, `Close(..)`, `Auth(..)`, `NegOpen{subscription_id, filter, initial_message: Cow<str>}`,
  `NegMsg{subscription_id, message}`, and `NegClose{subscription_id}`.
- `RelayMessage` helpers: `ok(id, bool, msg)`, `event(sid, ev)`, `eose(sid)`, `notice(s)`, `closed(sid, s)`.
  The NEG variants are struct-like (`NegMsg{..}`, `NegErr{subscription_id, message}`) with no helper ctor.
- `MachineReadablePrefix` has **no `Closed` variant**. Use a string `"closed"` or `Custom(SingleWord)`.
  Variants include `Duplicate, Pow, Blocked, RateLimited, Invalid, Error, Unsupported, AuthRequired, Restricted, Custom`.
- `Output<T>` has fields `value`, `success`, and `failed` (not `val`).
- `Filter::match_event(&ev, MatchEventOptions::new())` works for in-memory checks. On Postgres we translate the filter to SQL.

### Postgres item query (design; SQL itself UNVERIFIED, not run against PG)
```sql
-- events(id BYTEA PRIMARY KEY CHECK (length(id)=32), created_at BIGINT NOT NULL CHECK (created_at >= 0), kind INT, pubkey BYTEA, ...)
CREATE INDEX events_kind_ts_id ON events (kind, created_at, id);       -- index-only scan for NEG-OPEN
SELECT created_at, id FROM events
 WHERE kind = ANY($1) AND ($2::bigint IS NULL OR created_at >= $2) AND ($3::bigint IS NULL OR created_at <= $3)
 ORDER BY created_at, id LIMIT $4;      -- LIMIT = MAX_NEG_ITEMS + 1 sentinel (the LocalRelay trick)
```
- `bytea` ordering is memcmp, which matches negentropy's lexical ID order. `seal()` re-sorts anyway.
- Store `created_at` as `BIGINT` and reject values `< 0` or `>= 2^63` at ingest. Negentropy uses u64, and `u64::MAX` is reserved.
- Support only `kinds/authors/since/until/ids` in NEG-OPEN filters. Answer anything else (tag filters, search)
  with `NEG-ERR "blocked: unsupported filter"`.
- Hardening, mirroring nostr-sdk LocalRelay defaults: at most 10 NEG sessions per connection; cap total
  items per connection; idle-timeout sessions with `NEG-ERR "closed: ..."`; subscription ID at most 64 chars.

---

## 6. Live relay data (2026-10-25, VERIFIED with the raw python probe + nostr-sdk dry-run)

Probe: `["NEG-OPEN","p1",{"kinds":[2003]},"6100000200"]`.

| Relay | NIP-77 | kind-2003 |
|---|---|---|
| wss://nostr.oxtr.dev | yes (strfry-style), first frame 499 869 B, 15 619 ids + 1 fp range | **56 921 ids** total (full dry-run sync, 2.6 s) |
| wss://relay.nostr.net | yes, same first frame as oxtr | same backend/data? (UNVERIFIED) |
| wss://offchain.pub | yes | 15 |
| relay.damus.io, nos.lol, relay.primal.net, nostr.mom | NOTICE `ERROR: bad msg: negentropy disabled` | REQ works (313 / 500+ / 500+ / 129 with limit 500) |
| relay.snort.social | NOTICE `Unknown message type: NEG-OPEN` | REQ works |
| nostr.wine | silent, so we time out | REQ works (55) |
| relay.nostr.band | handshake timeout | n/a |

Takeaways:
- Most big relays have NIP-77 **disabled**, so the crawler must treat REQ pagination as the primary
  path and negentropy as a bonus.
- Supporting relays can reveal a relay's whole kind-2003 ID set cheaply. That is useful for crawler
  dedup ("which IDs am I missing") followed by REQ `{"ids":[..]}` in batches of 100.
- The network-wide kind-2003 set is small: 57k IDs is about 2.3 MB as negentropy items, so per-session loading into a Vec is fine.

---

## 7. Phone to server sync design (recommendation for ARCHITECTURE.md)

1. The phone is the **initiator**, direction **Up only**. The filter is `{"kinds":[2003,2004]}`, plus `since` when useful.
2. **Keep the negentropy ID set similar on both sides.** The phone deletes cached rows by LRU and size cap.
   If deleted rows left the set, every reconciliation would push the server's whole ID list back as `need_ids`
   (wasted bandwidth). Keep a tiny `sync_ids(id BLOB PRIMARY KEY, created_at INTEGER, synced INTEGER)`
   table, which the cleanup never prunes (about 64 B/row), and build the negentropy storage from it.
   Event bodies can be evicted. Only rows with `synced=1` are ever evicted, so the server already has them.
3. Loop: `initiate`, then `NEG-OPEN`. For each `NEG-MSG`, call `reconcile_with_ids`. Push `have_ids` to an
   upload queue. **Ignore `need_ids`**, or count them for stats. On `None`, send `NEG-CLOSE`.
4. Upload `EVENT`s with an in-flight window of about 50–100. Set `synced=1` **only on `["OK",id,true,..]`**,
   which includes `duplicate:`. `OK false` keeps the row unsynced and records the reason.
   If an ID in `have_ids` has no body locally, log and skip it (the server lost data).
5. Fallback, used when NEG-ERR or NOTICE arrives or there is no NEG-MSG within 10 s: send
   `POST /api/v1/events/batch` with unsynced events, chunked, and mark synced per accepted ID from the response.
6. The server must enforce `Event::verify()` on every EVENT and batch item, as already required.

---

## 8. Gotchas checklist
- Yanked versions: never pin `negentropy` 0.5.0, 0.5.1 or 0.4.3. Pin `=0.5.2` or `^0.5.2`.
- `seal()` is mandatory. `insert` after seal fails. Frame limit must be 0 or `>= 4096`.
- `have_ids` and `need_ids` accumulate across rounds. Pass fresh vectors per round (as nostr-sdk does) or drain them.
- Hex: `message.len()` may be odd or contain non-hex. Return NEG-ERR, don't panic. A malformed NEG-MSG must kill only that sub, not the socket.
- `Client::sync` returns Ok when a relay fails. Check `Output.failed`.
- `nostr` without features `rand`/`os-rng` has no `Keys::generate()` (nostr-sdk enables them). The ffi crate needs them explicitly.
- **Build env:** `/usr/bin/cargo` and `/usr/bin/rustc` (Arch rust 1.99.1, no Android std) shadow
  rustup's `~/.cargo/bin` (1.99.0 with `aarch64-linux-android` and `x86_64-linux-android`). For
  Android builds use `PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865 cargo ndk -t arm64-v8a build`.
  **VERIFIED:** negentropy 0.5.2 + nostr 0.45.5 (secp256k1-sys C build) cross-compile cleanly for arm64-v8a.
- If we ever do embed `nostr_sdk::local_relay::LocalRelay` (feature `local-relay`): it exposes
  `take_connection(stream: impl tokio AsyncRead+AsyncWrite+Unpin, addr)` for an already-upgraded stream
  (crate example `examples/local-relay-hyper.rs` uses `hyper::upgrade::on(req)` + `TokioIo`). Doing this inside an axum handler is **UNVERIFIED**.
  Builder knobs: `.database(impl NostrDatabase)`, `.max_negentropy_items(n)` (default 50 000),
  `.max_negentropy_subscriptions(n)` (default 10), `.write_policy(..)`, `.query_policy(..)`, `.min_pow(d)`.
  `NostrDatabase` requires `backend, features, save_event, check_id, event_by_id, count, query, delete, wipe`.
  `negentropy_items` has a default impl that loads full events via `query()`, so override it.
