# DHT crawling: protocols, techniques, yields, ethics, home-network limits, Rust options

Research date: 2026-10-25. Scratchpad (full reports, scripts, raw data):
`/tmp/claude-1000/-home-dude-backups/4523d0ae-a215-4c5b-b5e1-5b80e2cf6cbf/scratchpad/bitmagnet/`
(`dht-protocol.md`, `rust-dht.md`, `dht-protocol/`, `rust-dht/`, `check-*`).
Related: [bitmagnet.md](bitmagnet.md), [bittorrent.md](bittorrent.md) (BEP 5/33 client already
in `core::liveness`), [metadata-enrichment.md](metadata-enrichment.md).

Nothing under `~/magnetar` was changed by the research. Not legal advice.

---

## 0. TL;DR
- **BEP 51 `sample_infohashes` is the only compliant way to enumerate infohashes.** It works from
  this laptop behind NAT with no inbound port (measured: ~1.5-2.3k unique infohashes per ~25 s
  run of 300-400 queries).
- **The hard part is metadata (BEP 9), not discovery.** librqbit resolved 3/40 and, on a re-run,
  10/40 random sampled hashes within 90 s (7.5-25%). Plan for a low hit rate.
- **Honour the 6 h interval**, use a BEP 42 ID, cap global packet rate; do not copy the
  magnetico neighbour trick or bitmagnet's 60 s re-sample.
- **Home router conntrack / CGNAT is the bottleneck,** not bandwidth.
- **Rust:** no crate gives BEP 51 + control without a whole client. Write our own KRPC node on
  `core::liveness::{bencode,dht}` plus a bounded BEP 9 fetcher. ~1.5-2k LOC + ~400 LOC tests,
  3-6 agent-days.

---

## 1. Protocols

### 1.1 BEP 5 (KRPC)
Bencoded UDP: `ping`, `find_node`, `get_peers`, `announce_peer`. Already covered in
[bittorrent.md](bittorrent.md) §3. Routing table K = 8 per spec (bitmagnet uses 80).

### 1.2 BEP 51 (`sample_infohashes`)
bittorrent.org/beps/bep_0051.html (Draft, last modified 2017-01-03), fetched 2026-10-25:
- Request `{id, target}`. Response `id, interval, nodes (nodes6 allowed), num, samples` (N × 20 B).
- `interval` range 0-21600 s inclusive; "An indexer should not send a new sampling request to
  this node for that period of time" (should, not must).
- A supporting node should always include `samples`, even empty — that is how support is detected.

**libtorrent (de facto server side)**, RC_2_0 `eb397173`:
- Max 20 samples, interval 21600 (`settings_pack.cpp`). Cached sample reused only if
  `interval > 0`, not elapsed **and** `count() >= 20` (`dht_storage.cpp:586-588`); a node with
  fewer than 20 infohashes rebuilds its sample per query. Values clamped to 21600/20 (`:182-183`).
- Peer entries expire 45 min after last announce (`:88` announce_interval 30 min, `:566` ×3/2).
  **So BEP 51 only reveals live, recently announced swarms.**
- `dht_enforce_node_id=false`, `prefer_verified_node_ids=true`, `restrict_routing_ips=true`
  (one slot per IP), `max_metadata_size` 30 MiB, `dht_upload_rate_limit` 8000.
- **Bans a sender above 5 packets/s per IP (10 s average) for 5 minutes** (`dht_block_timeout` 5*60).

### 1.3 BEP 9 + BEP 10 (metadata from peers)
- BEP 3 handshake with LTEP bit → BEP 10 extended handshake (message id 20) advertising
  `ut_metadata` and `metadata_size` → 16 KiB piece requests → `sha1(info) == infohash`.
- **BEP 9 sets no size limit.** Caps in practice: bitmagnet/magnetico 10 MiB, libtorrent 30 MiB,
  librqbit 32 MiB.
- v2 torrents (BEP 52) put a SHA-256 truncated to 20 B on the DHT; SHA-1-only verifiers reject them.
- BEP 9's own examples have errors: reject example uses `i1e` instead of `i2e`; the data
  example's `total_size` (3425 vs 34256) does not match its bencoding.

### 1.4 BEP 33 (scrape), BEP 42 (IP-bound node IDs), BEP 43 (read-only flag `ro=1`)
Use BEP 42 IDs and set `ro=1` when we do not want to be inserted into routing tables (for a
crawler that does not serve, this is the polite default).

---

## 2. Discovery techniques compared

| Technique | How | Compliance | Notes |
|---|---|---|---|
| **BEP 51 sampling** | Walk keyspace with `find_node`, `sample_infohashes` each node once per interval | Compliant if interval honoured | Used by bitmagnet (only source, `internal/dhtcrawler/sample_infohashes.go:57`) and the maintained magnetico fork. Works behind NAT. |
| Passive `announce_peer` capture | Be a well-placed node; record announces | Compliant if ID honest | Needs reachable UDP port. bitmagnet stores announces but never fetches them (`responder.go:85-98`). |
| **Neighbour / Sybil trick** | Forged ID = target's first 15 B + own 5 B; answer `get_peers` with empty nodes + token so peers announce to us | **Abusive** | magnetico ≤ 0.7.2 (`093d4c01`: `nodeID[:15]+trueNodeID[:5]`, 2000 neighbours, find_node every 1 s, routing table cleared each tick). `dht-crawler` 0.2.1 copies a 6-byte prefix (`src/node_id.rs:21`). |
| Fake replies | tgragnato/magnetico (`43e62e3a`, `dht/mainline/indexingService.go:266-305`) returns routing-table addresses as fake peers in `get_peers` and real addresses with random IDs in `find_node` | **Abusive** | Do not copy. |

Other notes:
- Original boramalper magnetico final (`5546a941`) has an all-zero node ID
  (`indexingService.go:60`) and a slicing bug `Samples[i:(i+1)*20]` (`:224`), so only the first
  sample per reply is correct.
- ~300,000 Sybils measured in Mainline in 2011 (Wang & Kangasharju, "Real-World Sybil Attacks in
  BitTorrent Mainline DHT").
- DHT size: 15-27 M users/day in 2013, ~10 M daily churn (Wang & Kangasharju, IEEE P2P 2013) —
  dated, no newer primary figure found.

---

## 3. Measured on this laptop (2026-10-25, home NAT, 192.168.1.59, no port forward)

### 3.1 BEP 51 support census
Polite: one `sample_infohashes` per node, 40 pkt/s, `ro=1`. Script `dht-protocol/scripts/bep51_census.py`;
data `census1.json`, `census2.json`, fact-check re-run `check-dht-protocol/census3.json` (N = 1000).
Combined ranges across the three runs (the original narrower ranges were corrected):

| Metric | Range |
|---|---|
| Responders that return `samples` | **~50-60%** |
| Responders answering like `find_node` (no support) | 33-37% |
| Error 204 "Method Unknown" | **5-17%** (mostly `lt`-tagged nodes, likely rakshasa libtorrent/rtorrent — UNVERIFIED mapping) |
| Contacted nodes silent | **36-48%** |
| Supporters that are libtorrent-rasterbar (`LT`) | 96-99% |
| Supporters advertising `interval=21600` | ~97% (1,071/1,091; 251/259) |

Bias: nodes came from other nodes' routing tables, so long-lived libtorrent nodes are
over-represented. No published support census found. Many `LT` nodes also reply without samples
(probably older libtorrent, UNVERIFIED). 7 nodes advertised `interval=10`, possibly bitmagnet
(its default) — inference only.

### 3.2 BEP 51 yield probe (Rust, zero-dependency, `rust-dht/proto/src/bin/sample.rs`)
`find_node` bootstrap from 4 routers (~9.8 s), then one `sample_infohashes` to each of the first N
responders, 6 s wait.

| run | phase-1 responders | queried | `r` | `e` 204 | silent | `r` w/ samples | unique infohashes |
|---|---|---|---|---|---|---|---|
| 1 | 351 | 300 | 155 | 12 | 133 | 115 | 2288 (not saved) |
| 2 | 466 | 400 | 135 | 12 | 253 | 93 | 1843 (`hashes1.txt`) |
| 3 | 410 | 400 | 112 | 39 | 249 | 75 | 1470 (`hashes2.txt`) |
| check A | 520 | 400 | 163 | – | – | 108 | 2156 |
| check B | 39 | – | 23 | – | – | 14 | 280 (poor bootstrap) |

- Samples per response: median 20 (max 20-22). `num` median 132-222 (one node claimed 20,566,199).
- Median `interval` 21600 s → each node is worth re-sampling only every 6 h.
- BEP 51 among `r` responders: ~61-70%. Yield depends strongly on bootstrap quality.

### 3.3 Metadata hit rate (librqbit 9.0.1 `list_only`, `rust-dht/proto-meta`)
DHT only, trackers off, IPv4, 5 s warm-up, 40 random sampled hashes in parallel, 90 s timeout each.
- Run 1: **3/40** (17.8, 79.5, 90.0 s; seen_peers 1-5). Routing table 71 nodes.
- Fact-check re-run: **10/40** (22.5-86.2 s, most 40-90 s; seen_peers 2-191). Routing table 87.
- **Plan on ~10-25% within 90 s, with multi-minute deadlines for the tail.** Not sized for hours.
- Likely causes of misses: dead/stale swarms (sample only says "announced ≤ 45 min ago"), no
  trackers, shared 250 qps limiter on a young routing table, NAT.

### 3.4 Published figures

| Source | Figure | Caveat |
|---|---|---|
| bitmagnet FAQ (`faq.md:58`, `:24`) | 100-1,000 torrents/min; ~80 GB per 10 M torrents | vendor claim |
| magnetico v0.8 author | ~8,000/h | |
| magnetico users | 5k/day → 35-40k/day after raising neighbour limit | neighbour trick |
| MagnetDB (arXiv 2501.09275) | 28.6 M torrents, 950.6 M files, 300 weeks (~13.6k/day), `indexer-max-neighbors=10000`, **~30 TB/month** (~93 Mbit/s, 2:1 down:up) | |
| Grunthal 2010 (arXiv 1009.3681 §4) | 587k .torrents/week, 2500-3000 pps, 4.8-5 Mbit/s, 129 IPs (65 v6 + 64 v4), 10-20× normal node load | DB pre-filled with 5 M known hashes |
| Wolchok & Halderman 2010 | > 1 M torrents in < 2 h with 4,000 Sybils | **Vuze DHT**, not Mainline |

Metadata success (only 2010 data): ~19% (Grunthal, Mainline); 26.1% (Wolchok, Vuze; 82%
peer-weighted). Misses are mostly 1-3-peer swarms.

---

## 4. Ethics and politeness rules for Magnetar
1. Respect the BEP 51 `interval` (bitmagnet's 60 s override at `sample_infohashes.go:62-66` is the
   anti-pattern).
2. Honest BEP 42 node ID, persisted; `ro=1` unless we deliberately serve.
3. Global token bucket, and stay ≤ 1 query/s per remote IP (libtorrent bans > 5 pkt/s per IP).
4. No forged neighbour IDs, no fake peer/node replies, no announce spam.
5. Mark non-BEP 51 nodes instead of evicting them; keep them for routing.
6. Rotate any token secret if we answer `get_peers`.
7. Content filter (CSAM keyword list, as in bitmagnet `classifier.core.yml:249-267`) **before**
   anything is stored. Real samples include adult and possibly illegal content.

### Legal (not legal advice)
- Metadata-only crawling transfers no content pieces, but our IP joins swarm handshakes and is
  visible to monitoring firms; bitmagnet's FAQ recommends a VPN (`faq.md:16`).
- **Publishing** an index is the riskier act: CJEU *Stichting Brein v Ziggo* (C-610/15, 2017)
  held a torrent index (The Pirate Bay) is a "communication to the public".
- Whether metadata-only crawling triggers ISP or Arcom notices: UNVERIFIED.

---

## 5. Home-network limits
- Bottleneck is router conntrack and packet rate. Reports: bitmagnet #299 (internet "tanked" at
  < 100 kB/s), magnetico #253 (table full at 15k of 16,384), crashed LTE modems, ISP limits, a VPS
  suspended for "(D)DoS".
- Each new remote node holds 30-120 s of UDP conntrack state on this kernel; RFC 4787 / RFC 5382
  require NATs to keep UDP ≥ 2 min and transitory TCP ≥ 4 min.
- Behind CGNAT with ~1,024 ports: ~4 new TCP fetches/s (arithmetic, not measured).
- Mitigations: low concurrency, fd limits, VPN (collapses to one flow), or a VPS.
- **Active sampling needs no inbound port** (measured). Passive announce capture would.
- UNVERIFIED: this ISP's CGNAT status, router conntrack size, whether inbound UDP is possible.

---

## 6. Rust options

### 6.1 What Magnetar already has
- `Cargo.lock`: librqbit, librqbit-dht, librqbit-peer-protocol, librqbit-core, librqbit-bencode all
  9.0.1; librqbit-utp 0.7.0; tokio 1.54.1. librqbit optional behind `downloads`
  (`crates/core/Cargo.toml:10,33`).
- `crates/core/src/liveness/bencode.rs` (186 lines): lenient bencode `Value` + encode/decode.
- `crates/core/src/liveness/dht.rs` (372 lines): BEP 33 scrape client — `scrape()` `:93`,
  iterative `walk()` `:260`, `get_peers` builder `:356`. Client-only, single IPv4 socket.

### 6.2 Crates

| Crate | Version / date | Licence | BEPs | Verdict |
|---|---|---|---|---|
| librqbit-dht | 9.0.1, 2026-08-20 | Apache-2.0 | 5, 32 | **Cannot crawl.** Modules private (`src/lib.rs:1-7`); closed private `enum Request {GetPeers, FindNode, Announce, Ping}` (`src/dht.rs:942-951`); unknown incoming methods bail (`src/bprotocol.rs:546`); no hook for incoming queries (`dht.rs:813` trace only, peer store ≤ 1000 peers within `00000fff…`, `peer_store.rs:105-111`); routing table read-only (`dht.rs:1369-1371`). 250 qps via env `DHT_QUERIES_PER_SECOND` (`dht.rs:80-95`), 10 s timeout (`lib.rs:22`), `get_peers` stream never ends (`dht.rs:228,339`). Fine for `get_peers` only. |
| librqbit (`list_only`) | 9.0.1 | Apache-2.0 | 9/10 | Metadata-only works: `AddTorrentOptions{list_only:true}` → `ListOnlyResponse{info_hash, info, only_files, output_folder, seen_peers, torrent_bytes}`, no announce, nothing added or written (`session.rs:262, 296-303, 1236, 1298`). But `Semaphore::new(128)` **per infohash** (`dht_utils.rs:42`), no global cap, 10 s per-peer timeouts, **no overall deadline**, 32 MiB cap (`peer_info_reader/mod.rs:87`). Good for on-demand single magnets, not bulk. |
| librqbit-peer-protocol | 9.0.1 | Apache-2.0 | codecs | Exports `Handshake`, `Message`, `extended::ut_metadata::{UtMetadata, UtMetadataData}`, `EXTENDED_UT_METADATA_KEY` — enough for a bounded BEP 9 fetcher without `Session`. aws-lc via `librqbit-core`→`sha1w`: UNVERIFIED. |
| mainline (pubky) | 8.0.1, 2026-09-30 | MIT | 5, 42, 43, 44; IPv4 only | `RequestFilter::allow_request` sees all incoming requests (`rpc/server.rs:36-39,151`) and is re-exported, but `RequestTypeSpecific` is not re-exported (only Debug-matchable); `rpc` private, closed request enum (`common/messages.rs:56-63`), so no `sample_infohashes`. Pre-release `ed25519-dalek`. Not adopted. |
| fx-torrent (yoep) | 0.10.2, 2026-09-12 | Apache-2.0 | 3,5,6,9,10,11,14,15,29,32,33,42,43,44,**51**,53,55 | Real BEP 51 (`scrape_info_hashes_from` `src/dht/tracker.rs:542`, `scrape_info_hashes` `:561`, per-node `info_hash_indexing_interval`). Whole client: 56k LOC, 32 deps, one maintainer. Feature-trimmed build UNVERIFIED. |
| dht-crawler (0xddy) | 0.2.1, 2026-07-30 | MIT | 5, 9/10 via `rbit` | Neighbour-ID trick; no BEP 51; also does active discovery and get_peers per README. Reference only. |
| rusty-magnet (nine9123) | `ca45b66`, created 2026-10-19 | **none** (all rights reserved) | 5, 51, 9/10 | bitmagnet-style Rust crawler; read for ideas only. Tuning: LOOKUP_TIMEOUT 12 s, QUERY_TIMEOUT 5 s, PARALLEL_PEERS 4, MAX_PEERS_TRIED 8, MAX_PEERS_PER_HASH 12, MAX_QUERIES_PER_LOOKUP 16, retry after 1 h (`crawler.rs:79-84, 761-762`). Crawler 854 + metadata 273 LOC. |
| 0xcaff/dht-crawler | last push 2023 | none | 5 | Abandoned. |
| krpc 0.2.0 | 2023 | MIT/Apache | encoding only | Stale. |

### 6.3 Recommended build (feature `dht-crawl`, server-only)

```
core/src/dht/ (feature dht-crawl, no DB)
  krpc.rs      messages incl. sample_infohashes + r/e, on liveness::bencode       ~250 LOC
  node.rs      UDP actor: tid map (+ source check), 5 s timeout, global token bucket,
               per-IP limit, BEP 42 id, emits Observed{infohash, src, kind}         ~300 LOC
  nodepool.rs  LRU of nodes, next_sample_at = now + interval, bep51 flag            ~150 LOC
  sampler.rs   due nodes → sample_infohashes(random target); find_node discovery   ~200 LOC
  lookup.rs    get_peers(infohash, budget), generalised from liveness::dht::walk    ~100 LOC
core/src/metainfo/fetch.rs  BEP 3+10+9, global semaphore, 4 peers/hash, 12-15 s/hash
                            (longer tail budget), 32 MiB cap, SHA-1 (+ v2 later)     ~300 LOC
server/src/dht_crawler.rs   dedup (bloom + Postgres) → bounded queue → 32-64 metadata
                            workers → content filter → store → classify/enrich      ~400 LOC
server migrations           dht_torrents(...) + tsvector like D11                   ~100 LOC SQL
tests                       KRPC vectors, fake-peer fetcher, fake-socket sampler     ~400 LOC
```

Write BEP 9/10 from the spec, not from bitmagnet's `requester.go` (magnetico AGPL-derived) or
anacrolix (MPL-2.0). Do not run the crawler on the phone (battery, data).

**Effort:** KRPC/node/sampler 1-2 days; BEP 9 fetcher 1 day; daemon + schema 1-2 days; live
tuning 1 day → 3-6 agent-days. **Risks:** metadata hit rate, home-router load, illegal-content
filtering.

### 6.4 Design decisions for the user
- How DHT torrents fit D6 (Nostr trust model): a separate `dht_torrents` source joined by
  infohash with no trust score, or server-signed kind 2003 events.
- Passive capture needs a reachable UDP port; active sampling does not.

---

## UNVERIFIED
- Published BEP 51 support census (only our small biased one).
- Current Mainline DHT size (latest 2013).
- Share of peers reachable only via uTP or encryption.
- Whether metadata-only crawling triggers ISP/Arcom notices.
- ISP CGNAT status, router conntrack size, inbound UDP.
- Client mapping `lt` = rakshasa, `LT` = libtorrent-rasterbar; which libtorrent version added BEP 51.
- Whether `librqbit-peer-protocol` pulls aws-lc; whether fx-torrent trims to `dht` + `extension-metadata`.
- Steady-state metadata hit rate and throughput over hours.
- Cause of ~40-60% silent nodes.
