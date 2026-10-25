# BitTorrent research: tracker scrape (BEP 15/48), DHT (BEP 5/33), infohash and magnet

Researched 2026-10-25. Primary sources: bittorrent.org BEP 3/5/9/15/33/48/52/53 `.rst` files,
crates.io API, and the crate sources downloaded from crates.io (`mainline` 8.0.1,
`librqbit-dht`/`librqbit-core`/`librqbit`/`librqbit-tracker-comms` 9.0.1).
I tested everything against live networks from this machine, using 219 real kind-2003 events
fetched from nos.lol and relay.primal.net. The scratch scripts are in
`/tmp/claude-1000/magnetar-research/bittorrent/scripts/` (`udpscrape.py`, `dht33.py`, `fetch2003.py`).

## 0. Decisions (TL;DR)

| Topic | Decision |
|---|---|
| Tracker scrape | **Write our own small scraper in `core`** (`core::liveness::{udp, http}`). It is about 250 LOC on tokio `UdpSocket` + `reqwest`. No crate does scrape: `librqbit-tracker-comms` 9.0.1 has `// const ACTION_SCRAPE: u32 = 2;` commented out, so it only announces. |
| UDP batch size | **Chunks of 74 infohashes** (BEP 15 says "about 74"). Measured: opentrackr and torrent.eu.org answer at most **75** and **silently truncate** larger requests without returning an error. Map the response back by position and treat missing tail entries as "not answered", not as 0. |
| HTTP scrape | Use BEP 48 URL derivation (replace `announce` in the last path segment with `scrape`). Use a lenient bencode decoder, because real trackers emit unsorted dict keys. |
| DHT | **Primary: write our own BEP 33 `get_peers`+`scrape=1` lookup** (about 200 LOC: bencode + an iterative closest-node walk). It is the only DHT method that gives a swarm-size estimate. Neither `mainline` nor `librqbit-dht` sends `scrape=1` or parses `BFsd`/`BFpe`. Optional fallback: count distinct IPs from `librqbit_dht` `get_peers` (we already depend on it through librqbit). |
| DHT scheduling | Run a DHT scrape **only when no tracker scrape succeeded** (BEP 33 recommendation). Use low concurrency and lenient timeouts. |
| Aggregation | `seeders = max over trackers` and `leechers = max over trackers`. Do not sum: the swarms overlap. Store `source` (`udp`/`http`/`dht`), `checked_at` and the TTL. |
| Infohash canonical form | Store **lowercase 40-hex v1**. Accept 32-char base32 (case-insensitive) on input. Store v2 (`btmh`) as 64-hex sha256 in a separate optional column. |
| Magnet | `magnet:?xt=urn:btih:<hex40>&dn=<pct-enc title>&tr=<pct-enc url>...&xl=<total bytes>`. `xl` is optional: it is not defined in any BEP, but clients accept it. |

## 1. BEP 15 — UDP tracker protocol (verified spec + live test)

All integers are **big-endian**. "Do not expect packets to be exactly of a certain size": accept
longer packets, and check minimum lengths only.

### connect
```
request (16 B):  u64 protocol_id = 0x41727101980 | u32 action = 0 | u32 transaction_id
response (>=16): u32 action = 0 | u32 transaction_id | u64 connection_id
```
- The client may reuse `connection_id` for **60 s** after receiving it (trackers accept it for 2 min).
  After that, reconnect.
- Check: len >= 16, tid matches, action == 0.

### scrape
```
request:  u64 connection_id | u32 action = 2 | u32 transaction_id | N × [20]u8 info_hash
          size = 16 + 20N   (N=74 → 1496 B, which fits a 1500 MTU only at the UDP-payload level; measured OK)
response: u32 action = 2 | u32 transaction_id | N × { u32 seeders, u32 completed, u32 leechers }
          size = 8 + 12N
```
**Field order in the response is seeders, completed, leechers.** HTTP uses complete/downloaded/incomplete.
Check: len >= 8, tid matches, action == 2. Entries are positional, in request order.
`n_answered = (len-8)/12`.

### error
```
u32 action = 3 | u32 transaction_id | message bytes (UTF-8-ish, not NUL terminated)
```

### announce (only if ever needed; we do not announce for liveness)
IPv4 request is 98 B: `conn_id u64, action=1 u32, tid u32, info_hash[20], peer_id[20], downloaded u64,
left u64, uploaded u64, event u32 (0 none/1 completed/2 started/3 stopped), ip u32=0, key u32,
num_want i32=-1, port u16`. The response gives `action, tid, interval, leechers, seeders` and then
6-byte (v4) or 18-byte (v6) peers. The format follows the address family of the UDP socket.

### Timeouts / retransmit
The spec says `15 * 2^n` s, n = 0..8. That is far too slow for a UI. **Our choice**: 3 s timeout,
1 retry (a few seconds at most), then mark the tracker `unreachable` for this round and back off
that tracker for 1 h. UNVERIFIED that anyone needs longer; in tests the working trackers answered
in 30–170 ms.

### URL handling
- `udp://host:port/announce`. Ignore the path. BEP 41 lets you send the path as an option,
  but trackers do not need it for scrape. Resolve host to v4 and v6. Prefer v4 when both exist
  (simpler), or try both.
- Some events have `udp://host:port` without a path. Handle that too.

### Live measurement (2026-10-25, infohashes from real NIP-35 events)
| Tracker | Result |
|---|---|
| `udp://tracker.opentrackr.org:1337` | OK, 110 ms for 150 hashes (3 packets). 75-hash cap. 100 or 200 hashes → only 75 entries back, **no error** |
| `udp://tracker.torrent.eu.org:451` | OK, 50 ms. Same 75 cap with silent truncation |
| `udp://open.stealth.si:80` | OK, 170 ms |
| `udp://exodus.desync.com:6969` | timeout |
| `udp://open.demonii.com:1337` | timeout |

All 219 real events carry the same 4 trackers (`open.stealth.si:80`, `tracker.opentrackr.org:1337`,
`exodus.desync.com:6969`, `tracker.torrent.eu.org:451`, plus `http://nyaa.tracker.wf:7777/announce`
for 204 of them). So per-tracker batching across many torrents is very effective: group infohashes
**by tracker** and do one connect plus ceil(N/74) scrapes.

### Rust sketch (core::liveness::udp)
```rust
const PROTOCOL_ID: u64 = 0x41727101980;
const ACT_CONNECT: u32 = 0; const ACT_SCRAPE: u32 = 2; const ACT_ERROR: u32 = 3;
pub const UDP_SCRAPE_BATCH: usize = 74;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrapeStats { pub seeders: u32, pub leechers: u32, pub completed: u32 }

async fn udp_scrape(sock: &tokio::net::UdpSocket /* connected */, hashes: &[[u8;20]])
    -> Result<Vec<Option<ScrapeStats>>, ScrapeError>
{
    let tid: u32 = rand::random();
    let mut req = Vec::with_capacity(16);
    req.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    req.extend_from_slice(&ACT_CONNECT.to_be_bytes());
    req.extend_from_slice(&tid.to_be_bytes());
    let resp = send_recv(sock, &req, Duration::from_secs(3), 1).await?;   // retries once
    // check len>=16, action, tid → conn_id = u64::from_be_bytes(resp[8..16])
    let mut out = Vec::with_capacity(hashes.len());
    for chunk in hashes.chunks(UDP_SCRAPE_BATCH) {
        let tid: u32 = rand::random();
        let mut req = Vec::with_capacity(16 + 20 * chunk.len());
        req.extend_from_slice(&conn_id.to_be_bytes());
        req.extend_from_slice(&ACT_SCRAPE.to_be_bytes());
        req.extend_from_slice(&tid.to_be_bytes());
        for h in chunk { req.extend_from_slice(h); }
        let r = send_recv(sock, &req, ..).await?;
        // action==3 → Err(Tracker(String::from_utf8_lossy(&r[8..])))
        let n = (r.len() - 8) / 12;
        for i in 0..chunk.len() {
            out.push((i < n).then(|| { let o = 8 + 12*i; ScrapeStats {
                seeders: be32(&r[o..]), completed: be32(&r[o+4..]), leechers: be32(&r[o+8..]) } }));
        }
    }
    Ok(out)
}
```
Gotchas:
- Use one socket per tracker, or match on `(src addr, tid)`. Discard datagrams with the wrong tid.
- A responding tracker returns `0,0,0` for unknown infohashes. You cannot tell "unknown" from "dead
  swarm", so both count as dead.
- On Android/Termux, plain UDP sockets work. On mobile data, NAT can drop the reply. Handle it as a timeout.

## 2. BEP 48 — HTTP scrape (verified spec + live test)

- **URL derivation**: find `announce` in the **path** and replace it with `scrape`. Per the spec,
  only that method is in scope. A common rule (libtorrent/Transmission behavior, UNVERIFIED in
  BEP text) is that the *last path segment must start with* `announce`. Otherwise the tracker
  does not support scrape. Keep `announce.php` → `scrape.php` and keep the query string
  (passkeys). Example: `http://nyaa.tracker.wf:7777/announce` → `http://nyaa.tracker.wf:7777/scrape`.
- **Request**: `GET <scrape>?info_hash=<%-encoded 20 raw bytes>&info_hash=...`. Percent-encode
  every byte that is not unreserved (`[A-Za-z0-9.-_~]`). If the scrape URL already has `?`, append
  with `&`. Batch about 50–74 per request to keep the URL under about 2 KB (each hash is up to 60 chars).
- **Response**: `d5:filesd 20:<raw hash> d8:completei..e10:downloadedi..e10:incompletei..e e e e`
  - `complete` = seeders, `incomplete` = leechers, `downloaded` = ever completed.
  - Failure: `d14:failure reason<str>e`. Some trackers also add `flags`/`min_request_interval`. Ignore unknown keys.
- **Live**: `http://nyaa.tracker.wf:7777/scrape` returned **unsorted keys**
  (`complete, incomplete, downloaded`). This violates canonical bencode, so a strict decoder such as
  `bendy` in strict mode would reject it. `http://tracker.opentrackr.org:1337/scrape` works.
  `https://tracker.opentrackr.org/scrape` → **HTTP 410 Gone**.
- Decoder: write a minimal lenient bencode decoder (we need one for the DHT anyway, about 80 LOC), or use
  `serde_bencode` 0.2.4 (last release 2023-10-09, unmaintained-ish; it is tolerant of key order
  for deserialization, UNVERIFIED for unsorted input). **Decision: our own `core::bencode` module**,
  shared by HTTP scrape and the DHT.

## 3. BEP 5 — DHT (KRPC), relevant subset

- Bencoded dicts over UDP. Common keys: `t` (transaction id, 2 bytes is enough), `y` (`q`/`r`/`e`),
  `v` (optional 4-char client version, e.g. `"MG01"`. Do not rely on peers sending it).
- Errors: `e: [code, msg]` where 201 generic, 202 server, 203 protocol, 204 method unknown.
- Compact peer = 6 B (ipv4 + port BE). Compact node = 26 B (20 B id + 6 B). IPv6 (BEP 32) uses
  `nodes6`/38 B. The v6 DHT is optional for us.
- `get_peers`: `a = {id, info_hash}` → `r = {id, token, values: [6B...]}` or `r = {id, token, nodes}`.
  Lookup is iterative: always query the 8 known nodes closest by XOR distance to `info_hash`, and stop
  when the 8 closest have answered or the time budget runs out.
- Bootstrap nodes (from crate sources): `router.bittorrent.com:6881`, `dht.transmissionbt.com:6881`,
  `dht.libtorrent.org:25401` (librqbit-dht `DHT_BOOTSTRAP`), `router.utorrent.com:6881`.
- Each node returns only about 20 `values`, so **distinct `values` IPs heavily undercount** large swarms.
  Measured: for a swarm with 491 seeders + 256 leechers per tracker, I got 127 distinct IPs from values.

## 4. BEP 33 — DHT scrape (verified spec + test vector + live test)

- Request: add `"scrape": 1` (and optionally `"noseed": 1`) to the `a` dict of `get_peers`.
- Response, *if the node stores peers for that hash*: `r.BFsd` (seeds) and `r.BFpe` (peers), each
  a **256-byte** bloom filter. Older nodes return only `values`. BEP 33 then suggests building a local
  filter from those IPs and treating them as peers.
- Bloom: m = 2048 bits, k = 2. `h = sha1(ip_bytes)` (4 or 16 raw bytes, **no port**);
  `i1 = (h[0] | h[1]<<8) % 2048`, `i2 = (h[2] | h[3]<<8) % 2048`; set bit `bf[i/8] |= 1 << (i%8)`.
- Union = bitwise OR of all filters received during the lookup.
- Estimate: `c = min(m-1, zero_bits); size = ln(c/m) / (k * ln(1 - 1/m))`. Saturates around 8000
  entries, so show "8000+".
- **Test vector (verified in Python)**: insert 192.0.2.0–192.0.2.255 and 2001:db8::–2001:db8::3e7
  → filter begins `F6C3F5EA A07FFD91 ...`, estimate **1224.9308** (I got 1224.9309, which is a rounding
  difference). Put this as a unit test in `core`.
- **Live test** (own Python lookup, 25 s budget, about 170 nodes queried):
  - Hash with tracker numbers 491 seeders/256 leechers: 38 nodes returned BF. Estimates were **seeds 97,
    peers 811**.
  - Hash with tracker numbers 26/33: 13 nodes returned BF. Estimates were seeds 9, peers 77.
  - Interpretation: `BFsd` undercounts, because many clients do not send `seed=1` on announce. `BFpe`
    is in the same order as the tracker total. **Display the DHT result as "~N peers (DHT)"**,
    with `est = BFpe ∪ BFsd` (OR both, then estimate). Do not claim exact seeders.
- Scheduling (from the BEP): only DHT-scrape when no tracker or tracker scrape fails. Limit to 4 concurrent
  at startup and 1 steady. Use about 3 concurrent RPCs and a 10 s timeout. Randomise intervals. Cache the closest node
  lists per infohash for re-scrapes.

### Rust sketch (core::liveness::dht)
```rust
pub struct Bloom256(pub [u8; 256]);
impl Bloom256 {
    pub fn insert_ip(&mut self, ip: &std::net::IpAddr) {
        let b: Vec<u8> = match ip { IpAddr::V4(v) => v.octets().to_vec(), IpAddr::V6(v) => v.octets().to_vec() };
        let h = sha1_smol::Sha1::from(&b).digest().bytes();
        for i in [(h[0] as usize | (h[1] as usize) << 8) % 2048, (h[2] as usize | (h[3] as usize) << 8) % 2048] {
            self.0[i / 8] |= 1 << (i % 8);
        }
    }
    pub fn union(&mut self, o: &[u8]) { for (a, b) in self.0.iter_mut().zip(o) { *a |= *b; } }
    pub fn estimate(&self) -> f64 {
        let m = 2048.0; let zeros = self.0.iter().map(|b| b.count_zeros()).sum::<u32>() as f64;
        let c = zeros.min(m - 1.0);
        (c / m).ln() / (2.0 * (1.0 - 1.0 / m).ln())
    }
}
```
Lookup: one tokio `UdpSocket` bound to `0.0.0.0:0` (client-only. Do not answer queries, so we are not a
"server" node). Use a random 20-byte node id and a candidate map `addr → node_id` sorted by XOR distance.
Each round sends `get_peers{id, info_hash, scrape:1}` to the 8 closest unasked nodes (max 3–8 in flight).
Collect `nodes` (26-byte chunks, skip port 0), `values` (IPs), `BFsd`, `BFpe`. Stop when the 8 closest known
have all answered, or at 10–15 s. Dependencies: `sha1_smol` 1.0.1, `rand`, tokio. No DHT crate needed.

## 5. DHT crate evaluation

### `mainline` 8.0.1 (pubky/mainline, by Nuh/Nuhvi; released 2026-09-30; MSRV 1.85; edition 2021)
- Deps: `ed25519-dalek 3.0.0-pre.1` (pre-release!), `serde_bencode 0.2.4`, `sha1_smol`, `flume`, `lru 0.18`,
  `getrandom 0.4`. Features: `default=["full"]`, `full=["async"]`, `async=["node","flume/async","dep:futures-lite"]`.
- API: `Dht::client()?` / `Dht::builder().port(..).bootstrap(&[..]).build()?` → `.as_async()` → `AsyncDht`.
  `AsyncDht::get_peers(info_hash: Id) -> GetStream<Vec<SocketAddrV4>>` (a `futures_lite::Stream`).
  The sync `Dht::get_peers` is **`#[deprecated]` in 8.x** ("use the async API via Dht::as_async()").
  `Id: FromStr` (hex) and `Id::from_bytes(&[u8])`.
- **No BEP 33** (I grepped for `scrape`/`BFsd`/`noseed`: none). **IPv4 only** (`SocketAddrV4`). It runs its own
  actor thread. Its main focus is BEP 44 (pkarr).
- Verdict: **not used.** It gives only the undercounting `values` path. It adds a pre-release ed25519 crate
  next to nostr's secp256k1 stack, and a second DHT stack next to librqbit's.

### `librqbit-dht` 9.0.1 (ikatson/rqbit; 2026-08-20; also pulled in by `librqbit` 9.0.1)
- `DhtBuilder::with_config(DhtConfig { bootstrap_addrs, listen_addr, peer_id, cancellation_token, .. })`
  → `Dht = Arc<DhtState>`. `dht.get_peers(info_hash: Id20, announce_port: Option<u16>) -> RequestPeersStream`
  (`Stream<Item = SocketAddr>`, dual-stack v4+v6). Bootstrap: `DHT_BOOTSTRAP`.
- `librqbit::Session::get_dht() -> Option<&Dht>`, so the **server can reuse the download session's DHT**.
- Gotchas: the stream runs `request_peers_forever`, so you **must** wrap it in `tokio::time::timeout`
  and drop it. Peers repeat: dedup with a `HashSet<IpAddr>`. Pass `announce_port: None` so you do not announce.
  No BEP 33 (only a commented-out scrape constant in tracker-comms).
- Verdict: **fallback only** ("≥N peers via DHT", lower bound). The custom BEP 33 lookup is primary.

### Conclusion
Custom code is simpler and more accurate. Tracker scrape needs about 250 LOC (UDP + HTTP), BEP 33 DHT needs
about 250 LOC (bencode + lookup + bloom). Both are plain tokio and cross-compile to Android with no C deps.
This keeps `core` light for the `ffi` crate on the phone. The phone does not need librqbit for liveness.

## 6. Infohash formats

| Form | Where | Length | Notes |
|---|---|---|---|
| v1 hex | NIP-35 `x` tag, `urn:btih:` | 40 | sha1 of bencoded info dict. **All 219 real events use lowercase 40-hex.** Normalise with `to_ascii_lowercase()` and validate `[0-9a-f]{40}`. |
| v1 base32 | legacy `urn:btih:` | 32 | RFC 4648 base32 (alphabet A–Z2–7, no padding needed since 20 B = 32 chars). Clients "should support" it (BEP 9). `data_encoding::BASE32` (2.11.1) is **uppercase only**, so uppercase the input first. Convert to hex for storage. |
| v2 | `urn:btmh:` | `1220` + 64 hex | multihash: `0x12` = sha2-256, `0x20` = 32-byte length. Full sha256 of info dict (BEP 52). For trackers, DHT and the handshake, v2 uses the hash **truncated to 20 bytes**. NIP-35 only defines `x` = v1, so v2 is out of scope except for parsing pasted magnets. |
| hybrid | both `xt` | — | BEP 9: btih and btmh may both appear for the same hybrid torrent. |

Validation in `core`: an `x` tag that is not 40-hex (after lowercase) or 32-base32 → reject the event
(reason `bad_infohash`).

## 7. Magnet URI construction (BEP 9 + BEP 53)

```
magnet:?xt=urn:btih:<hex40>&dn=<title>&tr=<tracker1>&tr=<tracker2>[&xl=<bytes>][&so=0,2,4-6]
```
- `xt` is mandatory. `dn` (display name) and `tr` (repeatable) are optional, as is `x.pe=host:port`
  (direct peer). Without `tr`, the client uses the DHT.
- `so` (BEP 53, draft) = zero-based file indices to select, with ranges inclusive.
- `xl` (exact length) **is not in any BEP**. It comes from the generic magnet scheme. Clients ignore it or use it.
  Emit it as the sum of the `file` tag sizes only when all files have sizes. Optional.
- Percent-encode `dn` and every `tr` value as a query component (`percent-encoding` 2.3.2 with a set
  that encodes `&`, `=`, `+`, `#`, `%`, space, non-ASCII, `:` and `/` too, for safety). Keep `xt` literal.
  Space → `%20` (not `+`).
- Tracker list = the event's `tracker` tags (dedup, keep order) plus optionally a configurable default list.
  Cap at about 10 to keep the URI short for Android intents.
- Parsing pasted magnets: `librqbit_core::magnet::Magnet::parse(&str)` accepts hex-or-base32 btih,
  `btmh:1220…`, `tr`, `dn`, `so`. It requires the `url` crate. On the phone, write a ~40 LOC parser in `core`
  instead.

```rust
pub fn magnet(ih_hex: &str, name: Option<&str>, trackers: &[String], total: Option<u64>) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let mut s = format!("magnet:?xt=urn:btih:{ih_hex}");
    if let Some(n) = name { s += "&dn="; s += &utf8_percent_encode(n, NON_ALPHANUMERIC).to_string(); }
    for t in trackers { s += "&tr="; s += &utf8_percent_encode(t, NON_ALPHANUMERIC).to_string(); }
    if let Some(x) = total { s += &format!("&xl={x}"); }
    s
}
```
(`NON_ALPHANUMERIC` over-encodes `-._~`, which is valid and harmless.)

## 8. Liveness subsystem contract (proposal for ARCHITECTURE.md)

```rust
pub enum LivenessSource { Udp, Http, Dht, DhtValues }
pub struct Liveness {
    pub infohash: String,            // lowercase hex40
    pub seeders: Option<u32>,        // None for DHT (only estimate known)
    pub leechers: Option<u32>,
    pub completed: Option<u32>,
    pub dht_peers_est: Option<u32>,  // BEP 33 union estimate, capped 8000
    pub source: LivenessSource,
    pub trackers_ok: u16, pub trackers_failed: u16,
    pub checked_at: i64,             // unix seconds
}
```
- TTL: alive (seeders > 0) → 6 h. Dead → 24 h. Error/unreachable → 1 h with backoff. "Hide dead" =
  `checked_at` within TTL AND `max(seeders, dht_peers_est) == 0`. Unknown is **not** dead.
- Batching job: collect stale infohashes, group by normalised tracker URL, then per tracker do
  connect + ceil(N/74) scrapes with concurrency of about 8 trackers. Result = max per field across trackers.
  Then DHT-scrape (1–4 concurrent) only for hashes where every tracker failed or none exist.
- Tracker health table: `tracker_url, last_ok, last_err, consecutive_failures, next_try_at`.
  Skip a dead tracker (for example `exodus.desync.com`, `open.demonii.com` today) for an exponential backoff of up to 24 h.

## 9. Side observations from real relay data (relevant to other agents)
- The 219 kind-2003 events seen were mostly a nyaa.si mirror bot. Tags: `title, x, file (name,size),
  tracker, t, i (nyaa:<id>, tcat:anime,video,raw), proxy ["proxy","web","https://nyaa.si/view/<id>"]`,
  and **`nonce` (NIP-13) with target difficulty 21**. Event ids start with `00000…`.
  Tracker tags include the `/announce` path.
- relay.damus.io sent a non-JSON/fragmented frame to my naive client. That was a client bug (no
  continuation-frame handling), not proof of anything about damus.
