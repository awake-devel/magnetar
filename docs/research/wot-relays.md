# WoT, relay discovery, NIP-02 / 65 / 19 / 51 / 13: research notes

Researched 2026-10-25. Primary sources: `raw.githubusercontent.com/nostr-protocol/nips/master/{01,02,13,19,48,51,56,65,85}.md`
(fetched today), the `nostr` crate source 0.45.5 (from the cargo registry), and **live relay probes** (python websockets,
scripts in `/tmp/claude-1000/magnetar-research/wot/probe/`). Anything I did not check against a source is marked **UNVERIFIED**.

## 0. Crate versions (crates.io API, 2026-10-25)

| crate | max_stable | notes |
|---|---|---|
| `nostr` | **0.45.5** (2026-09-11) | has `nips::{nip02,nip13,nip19,nip51,nip65}`; default feature `std` |
| `nostr-sdk` | 0.45.4 | pulls in `nostr-gossip` 0.45.1 (outbox helpers) |
| `nostr-relay-pool` | 0.44.3 | older line, ignore; use the sdk |
| `bech32` | 0.12.0 | only needed if we drop `nostr` from a crate |

Checked in a scratch crate (`/tmp/claude-1000/magnetar-research/wot/scratch`, `nostr = "0.45.5"`):
```rust
use nostr::prelude::*;                      // PublicKey, ToBech32, FromBech32, RelayUrl, Tag, Event, Kind
PublicKey::from_hex(h)?; pk.to_bech32()?;   // -> "npub1..."
PublicKey::from_bech32("npub1...")?;
PublicKey::parse(s)?;                       // accepts hex OR npub (verified) -> use for the user's root input
nostr::nips::nip13::get_leading_zero_bits(id_bytes) -> u8   // verified: 000000000e9d... => 36
event.check_pow(difficulty: u8) -> bool     // exists on Event and EventId
nostr::nips::nip65::extract_relay_list(&event) -> impl Iterator<Item=(RelayUrl, Option<RelayMetadata>)>
nostr::nips::nip65::{RelayMetadata::{Read,Write}, Nip65Tag}
nostr::nips::nip02::{Nip02Tag::PublicKey{public_key, relay_hint, alias}, Contact}
nostr::nips::nip51::{MuteList{public_keys, hashtags, event_ids, words}, Nip51Tag}
```

## 1. NIP-01 replaceable semantics (what matters for kinds 3 / 10000 / 10002)

- `10000 <= n < 20000 || n == 0 || n == 3` are **replaceable**: one per `(pubkey, kind)`, keep the latest.
- **Tie-break**: same `created_at` means keep the **lowest id** (lexical). Implement exactly that.
- Relays *SHOULD* return only the latest, but **in practice they don't**. In one live fetch of kinds 0/3/10002/10000 for 21 pubkeys
  over 5 relays, some `(pubkey,kind)` pairs came back in 2, 3, 4, 6 and even **19 versions**. Always resolve client side:
```rust
fn newer(a: &Event, b: &Event) -> bool {      // is `a` the one to keep over `b`?
    a.created_at > b.created_at || (a.created_at == b.created_at && a.id.to_hex() < b.id.to_hex())
}
```
- Storage: a `replaceable_heads(pubkey, kind, created_at, event_id)` table. Ingest kind 3/10000/10002 only if it is `newer` than the
  head. Then **replace** the derived rows (follows / mutes / relay list) in a single transaction.
- Also reject `created_at` too far in the future (e.g. > now + 15 min). Otherwise a bogus future list pins the head forever.
  (This is policy, not spec.)

## 2. NIP-02: kind 3 follow list

Spec (final): `tags = [["p", <32-byte hex pubkey>, <relay url or "">, <petname or omitted>], ...]`. Content is "not used".
New list overwrites old. New follows are *appended*, so tag order is chronological.

Real-data gotchas, all verified live:
- **Legacy content relay map**: 100 of 385 real kind-3 events had non-empty content shaped like
  `{"wss://nos.lol/":{"read":true,"write":true}, ...}`. That is the pre-NIP-65 relay list. Use it **only as a fallback** when the
  pubkey has no kind 10002. Parse it leniently and ignore it if it is not JSON.
- **Size**: median kind-3 JSON is about 48 KB and the max seen was **185 KB** (about 2000 p-tags; one root's follows have up to 1999).
  Set the websocket `max_size` to at least 1 MiB (I used 16 MiB) and the server relay's max message to at least 512 KiB.
- **Other tags appear**: `t`, `client`, `alt`, `e`, `a`. Filter on `tag[0]=="p"` only.
- 8 malformed p-tags out of 265k (missing or short value). Validate `len==64 && lowercase hex`. Don't trust it.
- **`nostr` crate strictness**: `Nip02Tag::try_from(&tag)` **fails on the whole tag** if the relay hint is non-empty but not a valid
  ws/wss URL (verified: `["p",pk,"not a url","bob"]` gives an Err; an empty hint is fine). For WoT, **parse `tag[1]` ourselves** and
  treat the hint as an optional extra, so a junk hint doesn't drop the follow.
```rust
fn follows_of(ev: &Event) -> Vec<[u8;32]> {
    let mut v: Vec<[u8;32]> = ev.tags.iter().filter_map(|t| {
        let s = t.as_slice();
        if s.first().map(String::as_str) != Some("p") { return None; }
        let h = s.get(1)?;
        if h.len() != 64 { return None; }
        let mut out = [0u8;32]; hex::decode_to_slice(h.to_ascii_lowercase(), &mut out).ok()?; Some(out)
    }).collect();
    v.sort_unstable(); v.dedup(); v.truncate(10_000); v   // policy cap; real max seen ~2000
}
```
- A self-follow is possible. Ignore edges where `follower == followee`.

## 3. NIP-65: kind 10002 relay list metadata

Spec (draft): `["r", <url>]` means read+write, `["r", <url>, "read"]`, `["r", <url>, "write"]`.
- Download events **from** a user: use their **write** relays. This is how the crawler finds a known publisher's 2003s.
- Download events **about** a user (mentions): use their **read** relays. This applies to kind 2004 comments that `p`-tag a publisher.
- When publishing: send to the author's write relays plus the read relays of each tagged user. Spread the 10002 to indexers.
- Lists *SHOULD* be small (2 to 4 per category).

Real-data gotchas, all verified live on publishers of kind 2003:
- **Abusive sizes**: two kind-2003 publishers had 10002 lists with **3753** and **2632** `r` tags, all `read`. Cap per pubkey.
  Recommendation: if there are more than 50 `r` tags, mark the list `oversized`, take **no write relays** from it, and add a spam
  penalty (section 7).
- **Duplicates differing only by trailing slash**: for example `wss://relay.damus.io` and `wss://relay.damus.io/` in the same
  list. `RelayUrl::parse` **does not** normalize this (verified: it keeps the slash). It does lowercase the scheme and host.
  So we need our own normalizer.
- Non-`r` tags appear inside 10002: `client`, `published_at`. Ignore them.
- `ws://...onion` and path URLs (`wss://nostr.wine/inbox`, `wss://relay.x/haven-marble-ivory`) appear. A path is significant, so
  keep it. Onion: skip unless Tor is configured.
- Unknown marker: `Nip65Tag::try_from` errors (verified: `"READ"` uppercase is rejected), and `extract_relay_list` silently drops
  that tag. Recommendation: lowercase the marker; if it is still unknown, drop the tag (same as the crate).
- Coverage among 21 real kind-2003 publishers: **17/21 have a 10002**, 15/21 have a kind 3, and the top bot publisher has
  **neither**. So outbox alone can't find every publisher (see section 5).

Normalizer (policy; mirrors what clients do):
```rust
/// Returns None for non-ws(s), localhost/private IPs (on server), onion (unless allowed), userinfo, > 255 chars.
fn normalize_relay(raw: &str, allow_onion: bool) -> Option<String> {
    let raw = raw.trim();
    let mut u = url::Url::parse(raw).ok()?;                  // `url` crate; RelayUrl wraps it too
    if !matches!(u.scheme(), "ws" | "wss") { return None; }  // https:// is rejected by RelayUrl as well
    if !u.username().is_empty() || u.password().is_some() { return None; }
    let host = u.host_str()?.to_ascii_lowercase();
    if host.ends_with(".onion") && !allow_onion { return None; }
    if host == "localhost" || host.ends_with(".local") { return None; }        // + IP private-range check
    if (u.scheme()=="wss" && u.port()==Some(443)) || (u.scheme()=="ws" && u.port()==Some(80)) { u.set_port(None).ok()?; }
    u.set_fragment(None); u.set_query(None);
    let mut s = u.to_string();
    while s.ends_with('/') && s.matches('/').count() > 2 { s.pop(); }          // strip trailing slash(es) incl. root "/"
    (s.len() <= 255).then_some(s)
}
```
(Sketch, not compiled. Unit-test it with these inputs: `"wss://relay.damus.io/"` gives `"wss://relay.damus.io"`;
`"WSS://Relay.Damus.IO:443/"` gives the same; `"https://x.com"` gives `None`.)

Parsed shape we store:
```rust
pub struct RelayListEntry { pub url: String, pub read: bool, pub write: bool }
// marker None => read=true, write=true ; "read" => read only ; "write" => write only
// duplicates after normalization: OR the flags together
```

## 4. NIP-19: npub (and friends)

- bech32 (**not bech32m**). HRPs: `npub`, `nsec`, `note`. TLV forms: `nprofile`, `nevent`, `naddr` (`nrelay` deprecated).
- TLV types: `0` special (32-byte pubkey for nprofile), `1` relay (ascii, may repeat), `2` author, `3` kind (u32 BE).
  Unknown TLVs **must be ignored**. Strings are limited to 5000 chars. That exceeds the classic bech32 90-char limit, so a strict
  bech32 lib needs the "no length limit" variant. The `nostr` crate handles this.
- Test vectors (verified in Rust with `nostr` 0.45.5):
  - `3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d` encodes to `npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6`
  - `npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg` decodes to `7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e`
- **Never** put npub in events, filters, or the DB. Store `BYTEA(32)` / hex. Use npub only in UI, API display fields and config input.
  The root config accepts hex, npub or nprofile; parse it with `PublicKey::parse` (and `Nip19Profile` for nprofile, whose relays
  become seed relays).
- **Android**: the `ffi` crate should export `npub_encode(hex) -> String` / `npub_decode(s) -> String` via uniffi, so Kotlin doesn't
  reimplement bech32.

## 5. Outbox model and relay discovery for a torrent crawler

There is **no NIP named "outbox"**. "Outbox model" (also called "gossip model") is the community name for using NIP-65 write relays
to read authors. The `nostr-gossip` 0.45.1 crate implements it: `NostrGossip` trait, `BestRelaySelection::{All{read,write,hints,most_received}, Read{limit}, Write{limit}, Hints{limit}, MostReceived{limit}}`,
`GossipAllowedRelays{onion: true, local: false, without_tls: true}` defaults, `GossipPublicKeyStatus::{Missing, Updated, Outdated{created_at}}`.
We can use it through nostr-sdk, or reimplement it (small). **Recommendation: our own small implementation in `core`.** We need
Postgres/SQLite-backed state and our own caps, and must avoid tying the Store trait to the sdk's gossip DB.

Our crawl problem has **two halves**:
1. **Known publishers** (anyone with at least one stored 2003, plus WoT d1): fetch `{"kinds":[2003],"authors":[...],"since":cursor}`
   from their **write** relays. Greedy set cover: sort relays by the number of uncovered authors they serve, pick until each author is
   covered by at least 2 relays, then cap at R_MAX (server 30 relays; phone 8). Batch authors at no more than 500 per filter (relay
   limits; UNVERIFIED per relay, so be conservative). Comments: `{"kinds":[2004],"#e":[torrent ids]}` (NIP-35 comments reference
   via e; see the NIP-35 notes) from the same relays plus the publisher's read relays.
2. **Unknown publishers**: outbox can't find them. **Live proof**: the dominant publisher has no 10002. Do a **relay sweep**:
   `{"kinds":[2003],"since":cursor,"limit":500}` against a discovered relay set, paginating backwards with `until`.
   - Relay set = seed list ∪ every URL seen in any stored 10002 (any marker) ∪ kind-3 legacy content maps ∪ `p`-tag relay hints ∪
     relay hints in torrent `e` tags. Rank by frequency × yield (number of new 2003s per query). Probe NIP-11 first (optional).
     Exponential backoff on errors and 503s, and a per-relay cursor.
   - Indexers that answered kind 0/3/10002 queries **today**: `wss://purplepag.es` (only stores 0/3/10002-type kinds; returned 0 for
     2003), `wss://user.kindpag.es`, `wss://indexer.coracle.social`, `wss://relay.primal.net`, `wss://nos.lol`.
   - Relays that returned kind 2003 today: `wss://nos.lol` (500, so limit-capped), `wss://relay.primal.net` (500),
     `wss://relay.snort.social` (500), `wss://relay.damus.io` (313; the next connection got **HTTP 503**, so it is flaky or
     rate-limited), `wss://nostr.mom` (129). `wss://relay.nostr.band` **timed out** in the handshake. Treat it as dead; don't hard-code it.
   - Snapshot of that sweep: 752 unique kind-2003 events from **21 pubkeys**. One key (`b5c3145f…45ff`, name "nmirror")
     posted **623 (83%)**, `dd840e43…ecb0` posted 78, and the rest posted 1 to 10 each.
3. **Metadata refresh** for every publisher and every WoT node: `{"kinds":[0,3,10000,10002],"authors":[batch]}` against the indexers.
   Use a TTL of 24 h for d1 and 7 d for d2 (policy). The cursor is the head's `created_at`.

Relay hygiene: skip `ws://` on Android (optional), skip `.onion` without Tor, give at most 2 concurrent subs per relay, honour
`CLOSED`/`NOTICE`, and use `max_size >= 1 MiB`.

## 6. NIP-51 mute list (kind 10000) and related signals

- Kind 10000 is replaceable. Public items in `tags`: `p` (pubkeys), `t` (hashtags), `word` (lowercase string), `e` (threads).
- **Private items** sit in `.content`, NIP-44 encrypted to self (legacy NIP-04 has `?iv=` in the ciphertext). **We can only decrypt
  the root user's own private mutes if we hold their nsec. We won't**, since there is no key storage in Magnetar. So: use **public**
  mute tags for everyone. Optionally, a future NIP-46/NIP-55 (Amber on Android) signer could decrypt the root's private list.
  UNVERIFIED for feasibility; out of scope.
- Real data: 11 of 15 publishers with a kind 3 also have a 10000. Most have `content` set (encrypted) and **0 to 5 public `p`**.
  Expect public mutes to be a sparse signal, so penalties from them should be strong but rare.
- Also useful, all verified in spec text:
  - Kind **30000** follow sets and kind **10000** are in NIP-51. Kind 30007 "kind mute sets" (`d` = kind string, e.g. `"2003"`)
    lets a user mute a pubkey **just for torrents**. Support it: treat `30007` with `d=="2003"` exactly like a 10000 `p` for
    our purposes.
  - **NIP-56 reports, kind 1984**: `["p", <pubkey>, <type>]` with types including `spam`, `nudity`, `malware`, `illegal`,
    `impersonation`, `profanity`, `other`. The spec suggests the "3+ of your friends reported it" kind of logic. Use reports **from
    WoT members only** (reports from strangers are free to forge).
  - **NIP-85 trusted assertions** (kind 30382, `d`=pubkey, `["rank","0..100"]`, `followers`, `reports_cnt_recd`...): a
    future option to import a provider's rank. Don't depend on it.
- Our own local overrides (never published): `block_pubkey`, `trust_pubkey`. These are stored in the Store and always win.

## 7. NIP-13 PoW as a trust input (summary; see the NIP-13 notes for depth)

- difficulty = leading zero bits of the event id. A `["nonce", <n>, <target>]` tag **should** commit to a target.
  Rule: **effective_pow = min(actual_bits, committed_target)**. If there is no commitment, use `actual_bits` but treat it as
  "uncommitted" (NIP-13 allows rejecting or ignoring it). PoW of a lucky low target should not count above its commitment.
- Live distribution over 752 kind-2003 ids: 129 have no or low PoW (0 to 7 bits, accidental), and **623 carry `nonce` with a committed
  target of 21**. Actual bits ranged 21 to 31. All of those are from the one bot key.
  So PoW **does** separate "made an effort" from "free spam", but here the effort-maker is a high-volume bridge bot.
  PoW is evidence of cost, not of quality.
- The bot also carries NIP-48 `["proxy", "web", "https://nyaa.si/view/<n>"]`. The NIP-48 form is `["proxy", <id>, <protocol>]`,
  so **this bot has the arguments reversed**. Parse proxy tags leniently: either element may be the URL. It is a useful
  "bridged from <site>" badge and the explanation should show it.

## 8. Web-of-trust design (graph)

Graph = directed follow edges from kind 3 (+ public mutes from kind 10000/30007:2003 + WoT-member 1984 spam reports).
Root `R` = user's configured pubkey (one root; allow a list later).

Live size measurement: root = one real publisher (427 follows). 247 of their follows had a kind 3 on purplepag.es / primal.
That gave **145,361 edges** and **25,095 unique depth-2 pubkeys**, fetched in about 6 s from 2 relays. Of those d2 keys, 12,350 are
followed by ≥2 d1, 5,483 by ≥5, 2,878 by ≥10 and 1,389 by ≥20. **Conclusion: depth 2 is cheap. Depth 3 would be millions of
nodes, so don't do it.** Phone: store only the d1 set plus d2 keys that appear as publishers (we score publishers, not the world).
The server can store the full d2.

Definitions (computed per root):
- `D1 = follows(R)`. Mutes only count if the muter ∈ {R} ∪ D1.
- `n2(P) = |{ f ∈ D1 : P ∈ follows(f) }|`. That is the number of my follows who follow P (only for P ∉ D1 ∪ {R}).
- `m1(P) = |{ f ∈ D1 : P ∈ mutes(f) }|`; `rootMuted(P) = P ∈ mutes(R)`.
- `r1(P) = |{ f ∈ {R} ∪ D1 : f filed kind 1984 type spam|malware|illegal against P }|`.

SQL (server, Postgres) for n2, where `follows(follower BYTEA, followee BYTEA, PRIMARY KEY(follower, followee))` has an index on `followee`:
```sql
SELECT f2.followee AS pubkey, count(*) AS n2
FROM follows f1 JOIN follows f2 ON f2.follower = f1.followee
WHERE f1.follower = $1 AND f2.followee <> $1
  AND f2.followee = ANY($2)            -- restrict to publishers being scored
GROUP BY f2.followee;
```
Phone (SQLite): the same query works. Restricting to the publisher list keeps it small.

## 9. Proposed explainable trust score

Score is an integer **0 to 100**. It is computed per publisher pubkey, then adjusted per event by that event's PoW.
`trust(event) = clamp(publisher_score + pow_bonus(event), 0, 100)`. Every term is stored as a factor row, so the UI can show
"why".

```
Overrides (short-circuit, factor shown alone):
  local block                       -> 0     code=BLOCKED
  local trust                       -> 100   code=TRUSTED
  P muted by root (10000/30007)     -> 0     code=ROOT_MUTED

Positive (cap sum at 100):
  ROOT        P == R                                 +100
  FOLLOW_D1   P in D1                                +70
  FOLLOW_D2   P in D2: 50 * (1 - exp(-n2/4))          0..+50   (n2=1: 11, 3: 26, 5: 36, 10: 46)
  POW         pow_bonus(event) = 25 * clamp((eff_bits - 8) / 16, 0, 1)   0..+25 (24+ bits = full; eff = min(bits, committed))
  PROFILE     has kind 0 AND kind 10002 (not oversized)  +5
  AGE         first_seen(P) older than 30 d           +5      (first_seen = min created_at we've stored, any kind)
  COMMENTS    WoT members (R/D1) left kind 2004 on P's torrents: +3 each, cap +10

Negative:
  MUTED_D1    -20 per D1 muter, cap -60
  REPORTED    -15 per WoT spam/malware/illegal report, cap -45
  OVERSIZED_RELAYLIST  10002 with > 50 r tags         -15
  BURST       > 200 kind-2003 in any 1 h window AND P not in D1   -10   (bridges and bots; mild, not fatal)
  DUP_INFOHASH  same x/btih published by >= 3 distinct pubkeys with different titles, and P is not the earliest  -10 (per event)
  NO_TRACKERS_NO_DHT_HINT  event has no tracker tags and no magnet tr  -0 (liveness handles it; listed for clarity, no penalty)
```

Default **threshold = 20** (user-configurable 0 to 100; 0 means show everything). How this plays out on the live data:
- An unknown key with no PoW scores 0 (+5 at most), so it is hidden.
- nmirror bot: not followed (0 WoT), 0 followers anywhere (`#p` query on 4 relays). PoW 21 committed gives
  `25*(13/16)=20.3`, so 20. BURST: 623 events in about 39 h (1792777817 to 1792917365), max **65 in any 1 h window** (verified), so BURST
  does not fire and the score stays at 20. **So the default threshold sits right on the edge for the biggest real
  catalogue.** Decision for ARCHITECTURE.md: either default the threshold to **15**, or exempt `proxy`-tagged bridges from BURST.
  Recommendation: exempt bridges (a NIP-48 proxy tag present) from BURST, keep the threshold at 20, and show a "bridged from nyaa.si"
  badge. The user can raise the threshold to hide bridges.
- `dd840e43…` (573 followers network-wide, 78 torrents): reaches 20+ only if ≥1 of the root's follows follows them.
  n2=1 gives 11, plus PROFILE 5, plus AGE 5, so 21.

Explanation payload (API JSON and the FFI record):
```rust
#[derive(Serialize, Deserialize, Clone, Debug, uniffi::Record)]
pub struct TrustFactor {
    pub code: String,      // "FOLLOW_D2", "POW", "MUTED_D1", ...
    pub delta: i32,        // signed points actually applied (after caps)
    pub detail: String,    // "followed by 3 of your follows: alice, bob, +1" / "PoW 21 bits (committed 21)"
    pub evidence: Vec<String>, // hex pubkeys / event ids backing it (UI turns pubkeys into npub/petname)
}
#[derive(Serialize, Deserialize, Clone, Debug, uniffi::Record)]
pub struct TrustScore {
    pub pubkey: String,            // hex
    pub score: u8,                 // 0..=100, publisher-level (without the per-event POW)
    pub factors: Vec<TrustFactor>, // sorted by |delta| desc
    pub root: String,              // hex root used
    pub computed_at: i64,
    pub graph_version: i64,        // bump when any D1 list or root list changes, to invalidate the cache
}
```
Use NIP-02 petnames from the root's kind 3 for display in `detail` (`~/erin` style is optional).

Storage (both stores; SQLite names identical):
```sql
CREATE TABLE replaceable_heads (pubkey BYTEA, kind INT, created_at BIGINT, event_id BYTEA, PRIMARY KEY(pubkey, kind));
CREATE TABLE follows (follower BYTEA, followee BYTEA, PRIMARY KEY(follower, followee));
CREATE INDEX follows_followee ON follows(followee);
CREATE TABLE mutes (muter BYTEA, target BYTEA, source_kind INT, PRIMARY KEY(muter, target, source_kind)); -- 10000 or 30007
CREATE TABLE reports (reporter BYTEA, target BYTEA, report_type TEXT, event_id BYTEA PRIMARY KEY, created_at BIGINT);
CREATE TABLE relay_lists (pubkey BYTEA, url TEXT, read BOOL, write BOOL, PRIMARY KEY(pubkey, url));
CREATE TABLE relay_list_meta (pubkey BYTEA PRIMARY KEY, oversized BOOL, source TEXT /* 'nip65' | 'kind3_content' */);
CREATE TABLE trust_cache (root BYTEA, pubkey BYTEA, score SMALLINT, factors JSONB /* TEXT in sqlite */, computed_at BIGINT,
                          graph_version BIGINT, PRIMARY KEY(root, pubkey));
CREATE TABLE local_overrides (pubkey BYTEA PRIMARY KEY, action TEXT CHECK (action IN ('block','trust')), created_at BIGINT);
```
Server ingest must re-verify signatures for these kinds too, because they drive trust. A forged kind 3 would let anyone inject follows.

Recompute: incrementally when a D1 member's kind 3/10000 head changes (bump `graph_version`) or the root changes (full recompute).
Otherwise score lazily on read, with TTL 6 h. Per-event POW is computed at ingest and stored on the torrent row (`pow_bits`, `pow_committed`).
Search filter "trust ≥ T" is then `publisher_score + pow_bonus >= T`. Store `trust_total` denormalized on the torrent row and refresh
it when the publisher's cache row changes, so it stays indexable for sort and filter.

## 10. Open decisions to record in ARCHITECTURE.md

1. Default trust threshold: 20, with bridges (proxy tag) exempt from BURST. Alternative: 15.
2. Own outbox implementation in `core` vs `nostr-gossip`. Recommendation: own implementation (small, store-backed).
3. Use `nostr` 0.45.5 types, but use **lenient own parsers** for p/r tags (the crate's typed parsers drop entire tags on junk hints
   or uppercase markers).
4. Relay URL normalizer as specified (trailing slash stripped). Persist only normalized URLs.
5. Depth limit 2. The phone stores d2 only for publisher keys.
6. Private mutes are ignored (no nsec). Kind 30007 with `d="2003"` counts as a mute.
7. nostr.band is not used (dead today). Seed relays: nos.lol, relay.primal.net, relay.snort.social, relay.damus.io, nostr.mom;
   indexers: purplepag.es, user.kindpag.es, indexer.coracle.social.

## UNVERIFIED list
- Per-relay max authors per filter and max limit. I used 21 to 427 authors successfully on purplepag.es and primal; 500 is a guess.
- NIP-46/NIP-55 signer for decrypting the root's private mutes on Android.
- Exact `nostr-sdk` gossip configuration API names (not inspected; we don't plan to use it).
