# Nostr NIPs for Magnetar — NIP-01 / NIP-35 / NIP-13 (+ real relay data)

Research date: 2026-10-25. Primary sources were fetched from
`https://raw.githubusercontent.com/nostr-protocol/nips/master/{01,10,11,13,22,35,45,48}.md`
and from the dtan reference client (`https://git.v0l.io/Kieran/dtan`, `src/nostr-torrent.ts`, `src/const.ts`, `src/relays.tsx`).
Real data came from a live crawl (scripts: `/tmp/claude-1000/magnetar-research/nips/crawl.py`, `analyze.py`, `probe.py`).
Fixtures: `docs/research/fixtures/kind2003_sample.jsonl` (52 events), `kind2003_sample.reasons.json` (why each event was picked),
`kind2004_sample.jsonl` (60 events). Every fixture line was checked: the id recomputes and the BIP340 signature verifies (0 failures).
Fixtures are unmodified raw relay JSON, one event per line.

---

## 1. NIP-01 — event format and id

### 1.1 Wire event
```json
{"id":"<64 lowercase hex>","pubkey":"<64 lowercase hex, x-only secp256k1>","created_at":<int unix seconds>,
 "kind":<int 0..65535>,"tags":[["name","value",...],...],"content":"<string>","sig":"<128 lowercase hex, BIP340 Schnorr>"}
```
- `sig` is a BIP340 Schnorr signature over the 32-byte id. The id is the message itself; do not hash it again.
- Kind ranges: **regular** `1000<=n<10000 || 4<=n<45 || n==1 || n==2`, so kinds 2003 and 2004 are regular and relays store every one of them.
  Replaceable: `10000..20000, 0, 3`. Ephemeral: `20000..30000`. Addressable: `30000..40000` (keyed by `d` tag).
- NIP-35 has no notion of edits. A torrent "update" is a new event with a new id. Dedup by infohash at our layer (see section 4.6).

### 1.2 Id = sha256 of the canonical serialization
```
[0,<pubkey lowercase hex>,<created_at number>,<kind number>,<tags array of arrays of strings>,<content string>]
```
- UTF-8, no whitespace.
- Escape exactly `\n` `"` `\\` `\r` `\t` `\b` `\f` in strings. Every other character is written verbatim, so no `\uXXXX` for non-ASCII and `/` is not escaped.
- NIP-01 says nothing about other control chars (0x00-0x1F apart from the listed ones). JSON forces them to be escaped, and serde_json writes `\u00XX`.
  Python `json.dumps(..., separators=(",",":"), ensure_ascii=False)` reproduced **all 29,710 ids** in the first crawl (27,220 kind 2003 and 2,490 kind 2004), so serde_json/Python-style output is what real events use.
- **Never re-serialize parsed tags through a lossy model.** Keep `tags: Vec<Vec<String>>` and `content: String` exactly as received.
  Tags can contain `\n`, `\t`, CJK and emoji (seen in real file names).
- Rust:
```rust
use sha2::{Digest, Sha256};
fn event_id(pubkey: &str, created_at: i64, kind: u16, tags: &[Vec<String>], content: &str) -> [u8; 32] {
    let v = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    Sha256::digest(serde_json::to_string(&v).unwrap().as_bytes()).into() // to_string = compact, no ws, no ascii escaping
}
// verify: secp256k1::XOnlyPublicKey::from_slice(pk) + Secp256k1::verification_only().verify_schnorr(&sig, &Message::from_digest(id), &pk)
```
  (rust-nostr's `Event::verify()` does both the id check and the sig check. Its exact API belongs to the rust-nostr research topic and is UNVERIFIED here.)
- Server ingest MUST recompute the id **and** verify the sig. Lowercase-hex check: the `ids`, `authors`, `#e` and `#p` filter values MUST be exact 64-char lowercase hex.

### 1.3 Tags
- `["name", "value", ...]`: the tag name is element 0 and the value is element 1. Relays index only **single-letter** tag names (a-zA-Z), and only the **first value**.
  So `#x`, `#t`, `#i` and `#l` are queryable for kind 2003. `title`, `file`, `tracker` and `size` are NOT queryable.
- Verified live: `{"kinds":[2003],"#x":["<hash>"]}` returned the event on nos.lol, relay.primal.net, relay.damus.io and relay.nostr.net.

### 1.4 Client → relay
- `["EVENT", <event>]`
- `["REQ", <sub_id>, <filter1>, <filter2>, ...]`. `sub_id` is a non-empty string of at most 64 chars, unique per connection. Sending a new REQ with the same sub_id replaces the old one.
- `["CLOSE", <sub_id>]`
- NIP-45 (optional): `["COUNT", <id>, <filters>...]` → `["COUNT", <id>, {"count": N, "approximate"?: bool, "hll"?: hex}]`.

Filter object:
```
{"ids":[..],"authors":[..],"kinds":[..],"#<letter>":[..],"since":int,"until":int,"limit":int}
```
- Fields inside one filter are ANDed. Values inside one list are ORed. Several filters in one REQ are ORed.
- An **empty list matches nothing** (MUST NOT be treated as absent), so never send `[]`.
- `since <= created_at <= until`: **both bounds are inclusive**.
- `limit` applies only to the initial query. The relay returns the newest `n` events ordered by `created_at` DESC, with ties broken by lowest id first.
  `limit:0` means no stored events, only live ones. Relays may return fewer events than asked.
- After the stored events the relay sends `EOSE`, then keeps streaming live events until CLOSE.

### 1.5 Relay → client
- `["EVENT", <sub_id>, <event>]`
- `["OK", <event_id>, <bool>, "<prefix>: <msg>"]`. The 4th element is always present. It may be empty when the bool is true.
- `["EOSE", <sub_id>]`
- `["CLOSED", <sub_id>, "<prefix>: <msg>"]`. Sent when the relay refuses a REQ or kills it.
- `["NOTICE", "<msg>"]`. Free-form.
- Standard prefixes: `duplicate`, `pow`, `blocked`, `rate-limited`, `invalid`, `restricted`, `mute`, `error`.
  Not standard, but seen live: `unsupported:` in a CLOSED example. strfry sends `["CLOSED","s","ERROR: bad req: unrecognised filter item: search"]` (upper-case ERROR, prefix not normalized).
  **Parse prefixes case-insensitively and tolerate unknown ones.**
- Also handle an `["AUTH", challenge]` (NIP-42) message that arrives unsolicited. Treat it as "the relay may require auth"; we crawl anonymously.

### 1.6 Relay limits (NIP-11, `GET https://host` with `Accept: application/nostr+json`)
`limitation`: `max_message_length`, `max_subscriptions`, `max_limit` (the relay silently clamps `limit` to it), `max_subid_length`, `max_event_tags`,
`max_content_length`, `min_pow_difficulty`, `auth_required`, `payment_required`, `restricted_writes`, `created_at_lower_limit`, `created_at_upper_limit`, `default_limit`.
Observed values:

| relay | software | max_limit | page actually returned for limit=500 / limit=5000 |
|---|---|---|---|
| nos.lol | strfry | 500 | 500 / 500 |
| relay.primal.net | (none) | – | 500 / 500 |
| nostr.oxtr.dev | strfry | 500 | 500 / 500 |
| relay.nostr.net | (none) | – | **100** / 100 |
| relay.damus.io | (none) | – | small and erratic / 313 |
| relay.snort.social | memlay (v0l) | 5000 | 500 total, no older history |
| nostr21.com | strfry | 10000 | – |

**Gotcha:** the page size is whatever the relay sends, not `limit`. Paginate until a page comes back empty. Never stop because "page < limit".

### 1.7 Pagination strategy (implemented and tested in crawl.py)
1. Send `REQ {kinds:[2003], limit:500}`, collect events until EOSE, then CLOSE.
2. Next page: `until = min(created_at in page)`. `until` is inclusive, so boundary events come back again; dedup them by id.
   If the new `until` is not strictly smaller than the previous one, use `prev_until - 1`. This avoids an infinite loop when more than a page's worth of events share one second.
   (The bridge bots publish many events per second.)
3. Stop when a page returns 0 new events.
4. Cursor per (relay, kind) = the newest `created_at` seen. On the next run, use `since = cursor - overlap` (e.g. 1h, because relays receive events late and `created_at` is client-chosen), then paginate backwards with `until` down to `since`.
   Keep a separate "backfill until" cursor for the history crawl that has not finished yet.
5. **relay.damus.io:** `COUNT kinds:[2003]` = 6,867, yet `until`-pagination returned only 23 events (pages of 6, 2, 0 …), and later the relay answered HTTP 503.
   The cause is UNVERIFIED (likely query-time or rate limiting). Treat damus as a low-yield relay and do not trust "0 results" as proof that history is exhausted.
   A fallback is to page by time windows: `since/until` windows of 1 day.
6. Throttle. The crawler should hold 1 connection per relay and run pages sequentially, with a backoff on `rate-limited` or `CLOSED`.

---

## 2. NIP-35 — Torrents (kind 2003) and comments (kind 2004)

### 2.1 Spec (verbatim essentials; status `draft` `optional`)
- `x`: the V1 BitTorrent infohash (as in `magnet:?xt=urn:btih:HASH`).
- `file`: `["file", "<full path, e.g. info/example.txt>", "<size in bytes>"]`.
- `tracker`: optional, repeatable.
- `title`: `["title","<torrent-title>"]` (appears in the example only).
- `content`: "long-description-pre-formatted". In practice this is Markdown, HTML or BBCode-ish text.
- `t` tags: free-form category hashtags such as `movie`, `tv`, `HD`, `UHD`.
- `i` tags with prefixes:
  - `tcat:` is a comma-separated category path (e.g. `tcat:video,movie,4k`) that SHOULD match newznab on a best-effort basis.
  - `newznab:<id>` (Prowlarr NewznabStandardCategory).
  - `tmdb:`, `ttvdb:`, `imdb:`, `mal:`, `anilist:`.
  - Second-level type: `tmdb:movie:693134`, `ttvdb:movie:290272`, `mal:anime:9253`, `mal:manga:17517`.
- Comments: **kind 2004** "works exactly like a kind 1 and should follow NIP-10". Marked e tags: `["e", <id>, <relay>, <"root"|"reply">, <pubkey>]`.
- Note that NIP-10 now says kind 1 replies MUST NOT be used for other kinds and points to NIP-22 (kind 1111). Real clients **also** publish kind **1111** comments on torrents, with `["K","2003"]` (seen on relay.nostr.net, nostr.mom and nostr.oxtr.dev).
  **Ingest both 2004 and 1111 with `#K:["2003"]`.**

### 2.2 Reference parser (dtan `NostrTorrent.fromEvent`) — legacy variants the spec omits
- It skips any tag whose `t[1]` is falsy.
- `title` → title. The last one wins.
- **`btih`** is accepted as an alias for `x` ("v0"). It was seen in 117 real events.
- `file` → `{name: t[1], size: Number(t[2])}`.
- `tracker`.
- `i` → `split(":", 2)`. In JS that keeps only the first 2 pieces, so `tmdb:movie:693134` → type `tmdb`, value `movie`. That is a dtan bug; **do not copy it**. Split on the first `:` only.
- `t` → a generic tag.
- **`imdb`** as a top-level tag ("v0").
- Category: when `tcat` is absent, dtan uses the first 3 `t` tags as the category path ("v0: ordered tags before tcat proposal").
- dtan's magnet builder: `magnet:?xt=urn:btih:<hash>&dn=<title>&tr=<enc>...`. When the event has no trackers, dtan falls back to a built-in list (`src/const.ts` `Trackers`).
- dtan's default relays (`src/relays.tsx`): `wss://nos.lol/`, `wss://relay.damus.io/`, `wss://relay.nostr.band/`.
- The dtan category tree (`src/const.ts`) gives the canonical `tcat` vocabulary:
  `video{movie{dvdr,hd,4k},tv{hd,4k}}`, `audio{music{flac},audio-book}`, `application{windows,mac,unix,ios,android}`,
  `game{pc,mac,psx,xbox,wii,ios,android}`, `porn{movie{dvdr,hd,4k},picture,game}`, `other{archive,e-book,comic,picture}`.
  Real data adds `anime,video[,raw|live-action|amv]`, `manga`, `audio,lossless`, `software`, `pictures` and `other`.

### 2.3 What real relays carry (crawl of 2026-10-25)
| relay | kind 2003 | kind 2004 | notes |
|---|---|---|---|
| **wss://nostr.oxtr.dev** | **56,921** (complete; 2023-03-28 → 2026-10-15; 271 authors) | 293 | strfry. No COUNT. The best archive. |
| **wss://relay.nostr.net** | **≥59,396** (600 pages × 100 did not reach the end; reached back to 2026-02-23; 59 authors) | 2,422 | Pages of 100. COUNT timed out for 2003 and returned 2422 for 2004. |
| wss://relay.damus.io | COUNT=6,867 (paginated: 23) | 0 | Erratic pagination, then 503. |
| wss://nos.lol | 6,786 (COUNT 6,786, matching) | 0 | strfry. |
| wss://relay.primal.net | 6,534 (COUNT matching) | 0 | |
| wss://relay.snort.social | 500 (recent window only) | 0 | memlay. **Supports NIP-50 `search`.** |
| wss://nostr.mom | 129 | 15 | strfry |
| wss://nostr21.com | 121 | 21 | |
| wss://nostr.wine | 55 | 13 | supports `search` |
| wss://nostr.land | 23 | 8 | NFDB |
| wss://offchain.pub | 15 | 0 | |
| wss://nostr.bitcoiner.social | 11 | 0 | |
| relay.nostrplebs.com / nostr.einundzwanzig.space / nostr-pub.wellorder.net | 2 / 1 / 1 | 2 / 0 / 0 | |
| purplepag.es, relay.noswhere.com | 0 | 0 | |
| relay.nostr.band, nostr.fmt.wiz.biz | handshake timeout | | dead/unreachable on test day |
| relay.dtan.xyz | self-signed TLS cert | | not usable without disabling verification |
| relay.0xchat.com, relay.nostr.bg | DNS NXDOMAIN | | |
| relay.mostr.pub | redirects to https://relay.ditto.pub/ | | |
| relay.lumina.rocks | HTTP 502 | | |

Union across all relays: **98,749 unique kind 2003 events, 291 authors, 98,225 distinct infohashes**. 301 infohashes appear in more than one event, up to 5 times.
7,662 events were published in the last 30 days. The volume is dominated by **bridge bots**:

- **Nyaa bridge** (many pubkeys, one per uploader group, e.g. SubsPlease, Erai-raws, ToonsHub, LoliHouse): `["i","nyaa:<id>"]`, `["proxy","web","https://nyaa.si/view/<id>"]`, `["nonce","<n>","21"]`.
  It also sends a fixed 5-tracker list, plus `i tcat:anime,video` and `t anime`/`t video`. Content is often `"#### No description."`.
  60,255 events carry `nyaa:`.
- **TPB bridge**: `["i","tpb:<id>"]` (15,253 events).
- **YGG/"u2p" bridge** (French): `["i","ygg:<id>"]` (18,729 events).
  - `["published_at","<unix>"]`, `["size","<bytes>"]`, `["nfo","<name>.nfo"]`.
  - `["l","u2p.cat:2189"]`, `["l","u2p.pcat:2188"]`, `["l","u2p.source:ygg"]`, `["l","u2p.seed:N"]`, `["l","u2p.completed:N"]`, and `u2p.option_*` labels.
  - French `t` values such as `filmvidéo`, `pc/platine/lecteur multimédia/etc`, `multi (français inclus)`.
  - **File tags as one element `"name;size"`** with no third element.
  - Content is HTML (`<span>`, `<img>`).
- Hand-published events from dtan or noStrudel: no nonce, sometimes `["alt","…\nTorrent published on https://dtan.xyz"]`, `["client","noStrudel",…]`, few or no `i` tags.

### 2.4 Deviations from the spec — the parser MUST handle all of these (counts are from the first 27,220-event sample)
| issue | real example / count | handling |
|---|---|---|
| `btih` instead of `x` | 117 events | Accept `x`, falling back to `btih`. |
| uppercase hash | `["btih","77DCBD45…"]` | Lowercase it. |
| junk after the hash | `["x","620032fc…a94c&dn"]` | Extract the first `[0-9a-fA-F]{40}` (or 32-char base32 → decode to hex). |
| 64-hex `x` | `84ccd75d…290b` (1 event) | A v2 (BEP 52) hash or garbage. Store it as `infohash_v2`/invalid. Do not build a btih from it; `urn:btmh:1220<hash>` is the v2 form, UNVERIFIED that it is valid here. |
| no `x` at all | the same 117 (only `btih`) | Reject the event if neither tag yields a hash. |
| several `x` | 0 seen | Take the first. |
| `file` with 2 elements `"name;size"` | 7,958 tags (u2p) | Split on the last `;` when the remainder is digits. |
| `file` name containing a whole folder listing with `\n\t\t` and "(163.5 MiB)" text | 7,171 tags (Nyaa bridge; the 1st file tag is a scraped HTML blob) | Keep it but trim, cap the length (e.g. 1 KiB), and exclude it from file-tree rendering if it contains `\n`. |
| `file` with a 4th element (sha256) | 1 | Ignore the extras. |
| file size `"0"` | 89 | Allowed, size unknown. |
| sizes are **strings**, always decimal ints when present | 140,130 ok, 0 floats | `u64::from_str`. On failure: size unknown. |
| no `file` tags | 12 events | Size = the `size` tag if present, else unknown. |
| torrent-level `["size","N"]` (non-spec) | 808 | Use it when the files are missing or sum to 0. |
| `["published_at","unix"]` (non-spec, NIP-23 style) | 776 | Optional "original publish time". Sort by `created_at`, not by this. |
| no `tracker` | 950 events | The magnet works via DHT. Optionally add a default tracker list. |
| broken trackers | `"udp: //tracker.torrent.eu.org: 451 / anunciar"` (machine-translated), `""`, `wss://…` (WebTorrent) | Validate with a URL parse. Keep only `udp`/`http`/`https` for scrape; keep `wss` for magnet display only. Drop the rest. Dedup them (1 event had duplicates). |
| `i` prefixes outside the spec | `nyaa:` 60k, `tpb:` 15k, `ygg:` 19k | Store generically as `(prefix, value)`. Split at the **first** `:` only, so `tmdb:movie:693134` → (`tmdb`, `movie:693134`). |
| spec prefixes are rare | `imdb:` 1,570, `tmdb:` 4, `anilist:` 2, `mal:` 1, `ttvdb:` 1, `newznab:` **0** | Don't rely on newznab. Derive the category from `tcat`, then `t`. |
| `tcat` vocabulary differs from dtan's tree | `anime,video` (most common), `manga`, `audio,lossless`, `pictures`, `software` | Map to Magnetar categories with a table; unknown → `other`. |
| `t` tags = free text, all lowercase in practice | 0 uppercase seen | Lowercase anyway. |
| `proxy` tag argument order **reversed** vs NIP-48 | real `["proxy","web","https://nyaa.si/view/…"]`; NIP-48 says `["proxy",<id>,<protocol>]` | Accept both orders: whichever element looks like a URL is the source link. |
| `l` labels with namespace `u2p.*` (NIP-32 style, without an `L` tag) | 7,430 | Optional. `u2p.seed`/`u2p.completed` are stale seeder hints; don't trust them for liveness. |
| `ygg` tag with one element | `["ygg"]` ×63 | Ignore. |
| non-media torrents | Bitcoin assumeUTXO snapshot with `txoutset_hash`, `height`, `base`, `sha256`, `n` | Fine, they are regular torrents. |
| huge events | max 183,714 bytes; 19 events over 64 KiB; up to 2,848 tags/2,832 files; content up to 61,415 chars | Set the WebSocket `max_message_size` to at least 1 MiB (we used 16 MiB). Store files in a child table. Cap the indexed content length. |
| empty content | 1,614 | OK. |
| `title` always present in this sample | 0 missing, 0 duplicated | Still fall back to the first file name, then the infohash. |
| same infohash published several times | 301 groups, up to 5 | One `torrent` row per infohash with N `event` rows. Pick the canonical event by trust score, then the earliest. |

### 2.5 Kind 2004 reality — mostly NOT torrent comments
- 2,490 unique 2004 events from 32 authors. **2,165 come from one pubkey (b37243d2…) and 190 from another (4acab95e…).** Their tags are only `["t","<64-hex>"]` and their content is `"<32hex>:<base64>"`.
  That is encrypted data from some unrelated app squatting on kind 2004. It is **not** a NIP-35 comment.
- Real comments use NIP-10 forms, all of which must be parsed:
  - `["e",id,"","root"]` (94)
  - `["e",id,"","reply"]` (27)
  - `["e",id]` with no marker (26). Treat as a reply to that id.
  - `["e",id,"","",<pubkey>,"root"]`: 6 elements, marker at index 5 (14, non-standard). Search the elements for `root`/`reply`.
- A tracker-status bot (`6cf684fc…`) posts kind 2004 "种子状态报告" (torrent status report) messages with `["e",<torrent id>]`.
- **Rule: accept a 2004 only if it has an `e` (or `E`) tag whose id is a known kind 2003. Otherwise drop it.** Only 19 of the 2,490 referenced a 2003 we had fetched.
- Kind 1111 (NIP-22): the root is `["E",id,relay,pubkey]` + `["K","2003"]` + `["P",pk]`, and the parent is `e`/`k`/`p`. Query it with `{"kinds":[1111],"#K":["2003"]}`.
  There are very few (≤7 per relay), and some are duplicated across relays.

### 2.6 Normalized model (suggested contract for core)
```rust
pub struct Torrent {           // one per (event id); grouped by infohash for display
    pub event_id: [u8;32], pub pubkey: [u8;32], pub created_at: i64,
    pub infohash: [u8;20],      // from x|btih, lowercase hex, validated
    pub title: String,          // title tag | first file | hex hash
    pub description: String,    // content (raw; render as markdown/plaintext, strip HTML)
    pub files: Vec<TorrentFile>,// {path: String, size: Option<u64>}
    pub total_size: Option<u64>,// sum(files) else `size` tag
    pub trackers: Vec<String>,  // validated, deduped, order kept
    pub categories: Vec<String>,// tcat path split on ','  else first 3 `t`
    pub hashtags: Vec<String>,  // t values lowercased
    pub external_ids: Vec<(String,String)>, // i tags split at first ':' (imdb, tmdb, nyaa, tpb, ygg, ...)
    pub source_url: Option<String>,         // proxy tag URL
    pub pow_bits: u8, pub pow_target: Option<u8>,
    pub raw: String,            // original JSON, for re-serving via our relay / NIP-77
}
```
Magnet: `magnet:?xt=urn:btih:<hex>&dn=<urlenc title>&tr=<urlenc tracker>...` (BEP 9/53).

---

## 3. NIP-13 — Proof of Work
- Difficulty = the **number of leading zero bits of the 32-byte id**. Example: `000000000e9d97a1…` = 36 bits and `002f…` = 10 bits.
- Mining tag: `["nonce", "<nonce counter>", "<target difficulty>"]`. Clients SHOULD commit to the target in element 2. A client MAY reject a note that has the difficulty but no commitment.
  Use `min(actual_bits, committed_target)` as the effective PoW. Spammers who get lucky do not get credit beyond their commitment.
- The NIP-01 id does not cover the sig, so PoW can be delegated. PoW proves work, not authorship.
- Rust:
```rust
fn leading_zero_bits(id: &[u8;32]) -> u32 {
    let mut n = 0; for b in id { if *b == 0 { n += 8 } else { n += b.leading_zeros(); break } } n
}
fn effective_pow(id:&[u8;32], tags:&[Vec<String>]) -> u32 {
    let actual = leading_zero_bits(id);
    match tags.iter().find(|t| t.first().map(|s| s=="nonce").unwrap_or(false)).and_then(|t| t.get(2)).and_then(|s| s.parse::<u32>().ok()) {
        Some(target) => actual.min(target), None => actual }   // or 0 if policy requires commitment
}
```
- Real data (27,220 events): 26,147 carry a nonce, **all with target 21**, and every one meets its target.
  Bits distribution: 0–3: 1,020; 4–7: 50; 20–23: 22,789; 24–27: 3,069; 28–31: 268; ≥32: 21.
  **The bridge bots are exactly the ones mining PoW.** PoW therefore does not mean quality here. Hand-published dtan/noStrudel events have 0 PoW.
  Weight PoW lightly in trust: it is an anti-spam floor, and web-of-trust should dominate.
- Relays may advertise `limitation.min_pow_difficulty` and reply `["OK",id,false,"pow: difficulty 26 is less than 30"]`.

---

## 4. Implementation checklist / gotchas
1. The WS client needs a large `max_message_size` (≥1 MiB, events reach 180 KB). Use ping/keepalive, a 10s connect timeout, and a 20s per-message read timeout.
2. Handle `CLOSED` and `NOTICE` in the middle of a page (strfry and nostr.wine send NOTICE for unknown commands like COUNT). Handle unsolicited `AUTH`.
3. Feature-detect COUNT (NIP-45): nos.lol, primal, damus, nostr.mom and nostr21 support it. oxtr ("unknown cmd"), snort ("Unknown message type") and nostr.wine (NOTICE) do not.
4. Feature-detect NIP-50 `search`: only relay.snort.social and nostr.wine accepted it. strfry relays return `CLOSED … unrecognised filter item: search`. Search stays local (our DB).
5. Seed relay list for 2003, in priority order: `wss://nostr.oxtr.dev`, `wss://relay.nostr.net`, `wss://nos.lol`, `wss://relay.primal.net`, `wss://relay.damus.io`, `wss://relay.snort.social`, `wss://nostr.mom`, `wss://nostr21.com`, `wss://nostr.wine`, `wss://nostr.land`, `wss://offchain.pub`, `wss://nostr.bitcoiner.social`.
   Then extend through NIP-65 discovery of the authors' relays.
6. Dedup: by event id at ingest, and by infohash at the presentation layer.
7. Verify sigs on ingest. All 400 events checked in the sample and all 112 fixtures verified, so there is no evidence of relays serving bad sigs. Verification is still required by our threat model.
8. `created_at`: no future-dated events were seen. Still reject anything more than 15 min in the future, and allow old ones (back to 2023-03).
9. Full-text indexing: titles are multilingual (CJK, French, Spanish). Postgres `simple` config + pg_trgm suits them better than `english`. SQLite FTS5 should use the `unicode61 remove_diacritics 2` or `trigram` tokenizer (decision for the search topic).
10. Kind 2004: drop events with no e/E reference to a known 2003. Also ingest kind 1111 with `#K:["2003"]`.

## 5. UNVERIFIED / open
- Why relay.damus.io pagination stops early (rate limit vs query budget).
- The full size of relay.nostr.net (it did not end after 59,396 events and 600 pages).
- Whether 64-hex `x` values are deliberate v2 hashes.
- rust-nostr helper names for id/sig verification (see the rust-nostr research note).
- The newznab mapping: no real events carry `newznab:`, so the mapping table must be hand-made from the Prowlarr `NewznabStandardCategory.cs` list.
