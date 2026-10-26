# DHT crawler (ARCHITECTURE.md D49–D58)

Design document for the native Mainline DHT crawler. Status: **stage 1 (discovery) built**:
KRPC node, BEP 42 ids, routing, crawl pool, BEP 51 sampler, passive capture, peer collection,
the migration of §2.3, triage, the persisted `dht_seen` queue, the `dht` CLI and the `serve`
daemon (§13 has what was built and measured). No metadata is fetched yet (stage 2), so
`dht_torrents` stays empty. Each section says which stage builds it (§12); later stages update
this file as they land.

Research behind it: [research/dht-crawling.md](research/dht-crawling.md),
[research/bitmagnet.md](research/bitmagnet.md), [research/bittorrent.md](research/bittorrent.md),
[research/rust-crates.md](research/rust-crates.md). bitmagnet (Go) is a reference for ideas only:
**no code is ported or translated**, and its BEP 9 fetcher is magnetico (AGPL-3.0) derived
(bitmagnet.md §0). Every protocol rule below was checked against the BEP text fetched on
2026-10-26 from `https://www.bittorrent.org/beps/bep_00NN.rst` (BEP 3, 5, 9, 10, 20, 27, 32, 42,
43, 51, 52) and against crate sources from crates.io.

## 0. User decisions (2026-10-25, binding)

| Decision | Where it lands |
|---|---|
| DHT results are mixed **equally** with Nostr results; trust filters must not hide them | §5 (D51) |
| Adult content is **tagged and hidden by default**; the user can show it | §6 (D52) |
| A **hard blocklist** for illegal content (CSAM) always applies, is never user-overridable; such torrents are dropped and their names never stored | §7 (D53) |
| Crawl from the **home IP** in **aggressive** mode: BEP 51 sampling + passive listening on an open UDP port, UPnP to open it | §3, §9 (D49, D54, D57) |
| **No disk cap** | §10 (growth is reported, not limited) |
| Rust only; licences MIT/Apache/BSD/ISC/Zlib/MPL, no GPL/AGPL | §3.1, §9 |

## 1. Measured starting point (2026-10-26)

Production database (`magnetar-db-1`, read-only `BEGIN READ ONLY … ROLLBACK`):

| | |
|---|---|
| events / torrent events | 506 802 / 506 319 |
| distinct infohashes (`meta.infohash_count` = `torrent_infohashes`) | 501 675 |
| infohashes with more than one event | 3 314 |
| publishers (`publisher_counts`) | 328 |
| infohashes by category | video 209 815, anime 100 358, book 64 045, audio 48 672, **porn 36 809**, other 23 575, application 10 507, game 8 163, picture 32 |
| torrent events by trust | 0: 367 392 (72.6 %), 1–19: 3 444, ≥ 20: 135 483 (`trust.threshold` unset, so 20) |
| events with no tracker tag | 345 963 |
| `infohash_v2` set | 0 |
| `torrent_attrs` / `liveness_schedule` / `liveness` / `torrent_media` | 501 675 / 501 675 / 145 503 / 277 356 |
| database | 8 242 MB; `torrents` 3 204 MB total (heap 672 MB, TOAST 1 819 MB, `search_tsv` GIN 366 MB, title trigram 125 MB) ≈ 6.6 KB per event, mostly descriptions (avg 3 347 chars) |
| Tantivy index | 474 MB |

Laptop network (2026-10-26): default gateway `192.168.1.1` is a **neufbox (SFR) with MiniUPnPd
1.9** whose `GetExternalIPAddress` returns a **public** address (no CGNAT). It answered a
**unicast** SSDP M-SEARCH sent to `192.168.1.1:1900` but **no** reply came back to the standard
multicast M-SEARCH (`239.255.255.250:1900`), most likely because the laptop's nftables ruleset
drops replies that do not match the multicast flow (not proven). Kernel: `nf_conntrack_udp_timeout`
30 s, `_stream` 120 s, `nf_conntrack_max` 262 144. The router's own conntrack size is unknown.
Nothing was mapped on the router (only discovery and `GetExternalIPAddress`, both read-only).

Earlier measurements reused here: BEP 51 from this laptop without an inbound port gave
1 470–2 288 unique infohashes per ~25 s probe of 300–400 queries; about 50–60 % of answering
nodes support BEP 51 and ~97 % of supporters advertise `interval = 21600`; librqbit fetched
metadata for 3–10 of 40 sampled hashes within 90 s (dht-crawling.md §3). bitmagnet v0.10.0 on
this laptop stored ~370 torrents/min (≈ 22 k/h) for 25 min, 6–15 % of a core, ~230 MB RAM,
≈ 4 Mbit/s down, ~8 KB per torrent (bitmagnet.md §10).

## 2. Data model (D50)

### 2.1 Goal

An infohash can be known from Nostr, from the DHT, or from both. Everything keyed by infohash
(search documents D43, release attributes D44, seed-check schedule D45, media links D40/D41,
stats D47) keeps working with the least change. The canonical display picks a Nostr event
when one exists (D6 unchanged among events). Nostr-only code (trust, WoT, publisher stats,
comments, the relay endpoint, NIP-77, phone sync) never sees DHT rows.

### 2.2 Options considered

| Option | Verdict |
|---|---|
| Server-signed kind-2003 events (a server key "publishes" each DHT torrent) | **Rejected.** Rows would land in `events`, so `/relay` and NIP-77 would serve them and anyone syncing could republish them to Nostr; a fake publisher would enter trust, WoT, `publisher_counts`, the crawler's relay ranking; ~10–20 k Schnorr signatures and raw JSON per hour for nothing. |
| DHT rows inside `torrents` with a synthetic `event_id` | **Rejected.** `torrents.event_id` is a FK to `events` (needs dropping), a pseudo pubkey leaks into publisher stats, trust recompute, `EventRef` lists and the `local_overrides` trigger; every Nostr-only reader would need a filter. |
| **Separate `dht_torrents` (one row per infohash) + `torrent_infohashes.dht` + a `torrent_rows` view for infohash-level readers** | **Chosen.** Nostr tables and FKs stay untouched; the infohash-level tables already hang off `torrent_infohashes`; about ten SQL readers switch from `torrents` to the view (§2.5). |

### 2.3 Migration `20261101000000_dht.sql` (stage 4)

```sql
-- An infohash row now exists while it has Nostr events OR a DHT row.
ALTER TABLE torrent_infohashes ADD COLUMN dht boolean NOT NULL DEFAULT false;  -- metadata-only (PG ≥ 11)

-- Verified metadata fetched from peers (BEP 9): name and files are bound to the infohash by SHA-1.
CREATE TABLE dht_torrents (
    infohash        text COLLATE "C" PRIMARY KEY,           -- v1, 40 lowercase hex
    infohash_v2     text,                                    -- 64 hex SHA-256, hybrid torrents only
    title           text NOT NULL,                           -- info.name (name.utf-8 preferred)
    files           jsonb NOT NULL DEFAULT '[]',             -- [{path,size}], padding files dropped, first 5000
    file_count      integer NOT NULL,                        -- real count (may exceed 5000)
    files_text      text NOT NULL DEFAULT '',                -- paths joined by '\n', first 64 KiB
    total_size      bigint NOT NULL,
    category        text NOT NULL,                           -- core::model::Category snake_case (§6)
    metadata_size   integer NOT NULL,
    piece_length    integer,
    first_seen      bigint NOT NULL,                         -- first time the hash reached triage
    fetched_at      bigint NOT NULL,
    discovered_via  text NOT NULL CHECK (discovered_via IN ('sample','announce','get_peers')),
    peers_seen      integer NOT NULL DEFAULT 0,              -- distinct peers known at fetch time
    classifier_version smallint NOT NULL,
    search_tsv      tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', regexp_replace(title, '[._\-\[\]()+]+', ' ', 'g')), 'A') ||
        setweight(to_tsvector('simple', regexp_replace(files_text, '[._\-\[\]()+/]+', ' ', 'g')), 'B')
    ) STORED
);
CREATE INDEX dht_torrents_search_tsv ON dht_torrents USING GIN (search_tsv);  -- SQL fallback (D11) only
CREATE INDEX dht_torrents_first_seen ON dht_torrents (first_seen);
-- No trigram index: it only serves the fallback's typo path; ~125 MB per 500 k rows is not worth it.

-- Hashes the crawler has triaged but not (yet) stored. Stored ones leave this table
-- (dht_torrents is their record); blocked ones go to blocked_infohashes instead.
CREATE TABLE dht_seen (
    infohash        bytea PRIMARY KEY CHECK (length(infohash) = 20),
    first_seen      bigint NOT NULL,
    last_seen       bigint NOT NULL,
    via             smallint NOT NULL,       -- 0 sample, 1 announce, 2 get_peers
    status          smallint NOT NULL,       -- 0 queued, 2 failed, 3 invalid, 4 private, 5 v2_only, 6 oversize, 7 skipped_known, 8 alias
    attempts        smallint NOT NULL DEFAULT 0,
    next_attempt_at bigint,                  -- NULL = never retry; a queued row holds a 1 h lease (§3.7)
    last_error      text                     -- short code: no_peers, connect, no_ut_metadata, reject, timeout, hash_mismatch, …
);
CREATE INDEX dht_seen_retry ON dht_seen (next_attempt_at) WHERE next_attempt_at IS NOT NULL;

-- D53: illegal content. Only the hash, never a name; from any origin.
CREATE TABLE blocked_infohashes (
    infohash     text COLLATE "C" PRIMARY KEY,
    blocked_at   bigint NOT NULL,
    rule_version smallint NOT NULL,
    origin       text NOT NULL CHECK (origin IN ('dht','nostr','sweep'))
);
-- Nostr events rejected by D53 (id only), so NIP-77 does not fetch them again on every sync.
CREATE TABLE blocked_events (
    id         text COLLATE "C" PRIMARY KEY,
    blocked_at bigint NOT NULL
);

-- Counters, schedule, stats and the index log, as torrent_counts_ins / liveness_schedule_ins do
-- for torrents (statement-level, transition tables). Trigger names keep the counts trigger
-- before the schedule trigger (same-event triggers fire in name order).
CREATE FUNCTION dht_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE fresh bigint;
BEGIN
    WITH up AS (
        INSERT INTO torrent_infohashes AS ti (infohash, events, dht)
        SELECT infohash, 0, true FROM new_rows ORDER BY infohash
        ON CONFLICT (infohash) DO UPDATE SET dht = true
        RETURNING (xmax = 0) AS inserted)
    SELECT count(*) FILTER (WHERE inserted) INTO fresh FROM up;
    IF fresh > 0 THEN
        UPDATE meta SET value = value + fresh WHERE key = 'infohash_count';
        PERFORM stats_bump('torrents_added', fresh);
    END IF;
    -- A batch whose rows all hit ON CONFLICT DO NOTHING still fires this statement trigger
    -- with an empty transition table: no zero rows in stats_series.
    SELECT count(*) INTO fresh FROM new_rows;
    IF fresh > 0 THEN
        PERFORM stats_bump('dht_torrents_added', fresh);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER dht_counts_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_counts_ins();

CREATE FUNCTION dht_schedule_ins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO liveness_schedule AS s (infohash, born, due_at)
        SELECT infohash, first_seen, -first_seen FROM new_rows ORDER BY infohash
        ON CONFLICT (infohash) DO NOTHING;      -- a Nostr row already scheduled it
    RETURN NULL;
END $$;
CREATE TRIGGER dht_schedule_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_schedule_ins();

CREATE FUNCTION dht_counts_del() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE gone bigint;
BEGIN
    UPDATE torrent_infohashes SET dht = false WHERE infohash IN (SELECT infohash FROM old_rows);
    WITH del AS (DELETE FROM torrent_infohashes WHERE events <= 0 AND NOT dht
                 AND infohash IN (SELECT infohash FROM old_rows) RETURNING 1)
    SELECT count(*) INTO gone FROM del;
    IF gone > 0 THEN UPDATE meta SET value = value - gone WHERE key = 'infohash_count'; END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER dht_counts_del AFTER DELETE ON dht_torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION dht_counts_del();

CREATE TRIGGER search_changes_ins AFTER INSERT ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_upd AFTER UPDATE ON dht_torrents
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_new_rows();
CREATE TRIGGER search_changes_del AFTER DELETE ON dht_torrents
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION search_changes_old_rows();

-- The Nostr delete trigger must not drop an infohash the DHT still holds:
CREATE OR REPLACE FUNCTION torrent_counts_del() …   -- same body as 20261028000000, with
    -- "DELETE FROM torrent_infohashes WHERE events <= 0 AND NOT dht AND infohash IN (…)"

-- Infohash-level readers: every source, Nostr rows first in canonical order (trust DESC).
CREATE VIEW torrent_rows AS
    SELECT event_id, pubkey, created_at, infohash, infohash_v2, title, description, files,
           file_count, files_text, total_size, trackers, category, category_path, hashtags,
           external_ids, source_url, published_at, pow_bits, trust, search_tsv,
           'nostr'::text AS origin
    FROM torrents
    UNION ALL
    SELECT ''::text COLLATE "C", ''::text COLLATE "C", first_seen, infohash, infohash_v2, title,
           '', files, file_count, files_text, total_size, '{}'::text[], category, '{}'::text[],
           '{}'::text[], '[]'::jsonb, NULL::text, NULL::bigint, 0::smallint,
           (-1)::smallint,                     -- below every publisher: Nostr wins canonical
           search_tsv, 'dht'::text
    FROM dht_torrents;

INSERT INTO meta (key, value) VALUES ('dht_blocklist_version', 0) ON CONFLICT (key) DO NOTHING;
```

The stage that writes it runs `backend-design:review-migration`-style checks: `ADD COLUMN …
DEFAULT false` with a constant default is metadata-only (production runs `postgres:18`,
`compose.yml`), but it still takes an `ACCESS EXCLUSIVE` lock on `torrent_infohashes` (501 675
rows), so it waits behind any open reader of that table and blocks everyone queued after it;
migrations run at `serve` start before the loops, and the migration sets `SET LOCAL
lock_timeout = '10s'` so a stray session (a manual `psql`, a dump) fails the start loudly
instead of stalling it. The view and empty tables take no time; `CREATE OR REPLACE FUNCTION
torrent_counts_del` replaces the function body only (no table lock; the trigger on `torrents`
picks up the new body on its next statement). Down-migration is not provided (sqlx migrations
are forward-only in this repo).

**Why `dht_seen.infohash` is `bytea`**: it is the crawler's private, possibly tens-of-millions-row
table (20 B key vs 41 B text). Every table shared with the rest of the server stays lowercase
hex `text COLLATE "C"` like `torrents`.

**Why no FK from `dht_torrents` to `torrent_infohashes`**: the parent row is created by the
statement trigger, after the insert; `torrents` has no such FK either. The children
(`torrent_attrs`, `liveness_schedule`, `liveness_history`) keep their FK to
`torrent_infohashes` and are written after the `dht_torrents` statement in the same transaction.

### 2.4 Writes (stage 4, `server::dht::persist`)

One transaction per batch (≤ 500 rows or 1 s): `INSERT INTO dht_torrents … ON CONFLICT
(infohash) DO NOTHING` → `INSERT INTO torrent_attrs … ON CONFLICT DO NOTHING` (release
attributes parsed from the DHT name + files with `core::release`, first writer wins as for
Nostr, D44) → `DELETE FROM dht_seen WHERE infohash = ANY(…)` over the hashes the fetches were
**triaged under** (the sighted hashes). Sighted and stored hash differ for a hybrid torrent
sighted under its truncated v2 hash (§4 step 5), which is stored under its SHA-1: that sighted
row is not deleted but set to status 8 `alias` with `next_attempt_at = NULL`. Deleting by the
stored hash would leave it `queued` forever; deleting it would let the truncated hash, absent
from `torrent_infohashes`, be fetched again every time it is sampled. The blocklist (§7) runs **before** the batch is
built; a blocked hash never reaches this code path.

### 2.5 Readers that switch from `torrents` to `torrent_rows` (stage 5)

| Reader | Why |
|---|---|
| `search_index/load.rs::load_docs` | documents: DHT-only infohashes get a document, mixed ones gain `origin` (§5) |
| `store.rs` `torrent_detail` (`:1239` area) | DHT-only detail (§5.3) |
| `search.rs` SQL fallback | DHT rows searchable while the index builds |
| `enrich/mod.rs` `due`, `load_groups`, `coverage` | IMDb/TMDB matching by name works on DHT names (D40 already uses the largest video file) |
| `enrich_anime.rs` pending query | anime by name (`category = 'anime'`, §6) |
| `attrs.rs` backfill | re-parse DHT names when `PARSER_VERSION` moves |
| `stats.rs::recompute_published` | not a switch but a fix: it counts `liveness_schedule.born` per day, which DHT rows now also fill. It must join `torrent_infohashes` and keep `events > 0`, or every DHT discovery shows up as a "published" torrent on its `first_seen` day (D47's chart means Nostr publications). For a hash known from both sources, `born` can be the DHT `first_seen` when that is earlier (the schedule trigger keeps the minimum); accepted |

Readers that look a row up **by `event_id`** stay on `torrents`: DHT rows have `event_id = ''` in
the view, and an empty id from a client must never resolve to an arbitrary DHT row.

Stay on `torrents` (Nostr-only): trust/publisher stats, `pow_bits`, comments, `known_torrent_event`,
`publishers()`, overrides trigger, WoT/relay graphs, the stats backfill from `events.received_at`,
`liveness.rs` tracker list (DHT rows have no trackers: the D45 checker already scrapes tracker-less
hashes on `magnet::DEFAULT_TRACKERS`, then BEP 33).

`events`, `/relay`, NIP-77 and phone sync never see DHT rows, so **nothing from the DHT is
republished to Nostr**. The phone does not cache server search results as events (`ffi::search`
returns them as is), so DHT results exist on the phone only while online; the offline cache stays
Nostr-only (documented limitation).

## 3. Crawler architecture (D49, D54, D55)

### 3.1 KRPC layer: our own, on `core::liveness::bencode`

`librqbit-dht` 9.0.1 (already in the tree through librqbit) was re-read in
`~/.cargo/registry/src/*/librqbit-dht-9.0.1`: modules are private (`src/lib.rs:1-7`); the
request enum is private and closed (`enum Request { GetPeers, FindNode, Announce, Ping }`,
`src/dht.rs:942`); incoming methods other than ping/find_node/get_peers/announce_peer fail with
`unsupported method` (`src/bprotocol.rs:546`); incoming `announce_peer` goes straight into its
private peer store (`src/dht.rs:840-845`, store limited to 1000 peers within
`00000fff…` of its id, `src/peer_store.rs:105-111`) with no hook; the rate limiter is a fixed
env-read 250 q/s (`src/dht.rs:80-95`). **It cannot send `sample_infohashes` nor report incoming
queries**, so it stays the liveness/download DHT only. `mainline` 8.0.1 has the same closed
request enum and a pre-release ed25519; `fx-torrent` has BEP 51 but is a 56 k-LOC client
(rust-crates.md, dht-crawling.md §6.2). Decision: our own KRPC node in
`core::dht` (feature `dht-crawl`, server-only, no new dependency except `crc32c` 0.6.8,
MIT/Apache-2.0, for BEP 42), reusing the lenient bencode of `core::liveness::bencode`.

Modules (stage 1 built; the planned `node.rs` and `sampler.rs` became `engine.rs`):

```
core/src/dht/          (feature "dht-crawl")
  krpc.rs        encode/decode: ping, find_node, get_peers, announce_peer, sample_infohashes; r/e;
                 compact nodes (26 B) / peers (6 B); `ip` (BEP 42), `ro` (BEP 43), `v`
  id.rs          BEP 42 node ids (CRC32C), test vectors from the BEP
  node.rs        one UDP socket per virtual node: tid map keyed (tid, addr), 4 s timeout,
                 shared rate limiter, query responder, token secret rotation
  routing.rs     BEP 5 K=8 bucket table per virtual node (good/questionable/bad, 15 min)
  pool.rs        crawl pool: nodes to sample, next_sample_at, bep51 state, bounded
  sampler.rs     due nodes → sample_infohashes(random target); keyspace traversal from `nodes`
  lookup.rs      iterative get_peers (generalised from liveness::dht::walk) for peer discovery
  peerstore.rs   honest store of announced peers (served back in get_peers / sample_infohashes)
  limiter.rs     global token bucket + per-remote-IP bucket + adaptive brake
core/src/metainfo/     (feature "dht-crawl", stage 2)
  wire.rs        BEP 3 handshake, BEP 10 extended handshake, BEP 9 ut_metadata messages
  fetch.rs       bounded fetcher: peers → verified info dict
  info.rs        info dict → name, files, size, private, meta version, v2 hash
core/src/classify.rs   category + adult tagging from name and files (stage 3, no feature: pure)
core/src/blocklist/    hard illegal-content matcher (stage 3, no feature: Nostr ingest uses it too)
server/src/dht/        daemon: mod.rs (pipeline, report), seen.rs (Bloom), queue.rs (fetch queue),
                       store.rs (triage, leases, identity) — stage 1; persist, upnp, stats (4, 6)
```

### 3.2 Modes (D49)

`MAGNETAR_DHT` = `off` (default) | `polite` | `aggressive`. The default stays off: crawling
the DHT makes the host's IP visible in swarms (dht-crawling.md §4 "Legal"), which every
deployment must opt into. The user's laptop runs `aggressive`.

| | polite | aggressive (user's choice) |
|---|---|---|
| Discovery | BEP 51 sampling only | BEP 51 sampling **and** passive capture of incoming `get_peers` / `announce_peer` |
| Answers queries | no: every query carries `ro=1` (BEP 43) and incoming queries are ignored | yes: full BEP 5 node (ping, find_node, get_peers, announce_peer) + BEP 51 responder; no `ro` |
| Virtual nodes | 1 | 4 (`MAGNETAR_DHT_NODES`, 1–8) |
| UDP port | ephemeral, no mapping | `MAGNETAR_DHT_PORT`…`+NODES-1` (default 6891–6894), UPnP-mapped (§9) |
| Outbound KRPC | 50 queries/s | 200 queries/s (`MAGNETAR_DHT_QPS`) |
| Metadata fetches in flight | 16 | 128 (`MAGNETAR_DHT_FETCH_CONCURRENCY`) |
| Crawl pool | 200 k nodes | 1 M nodes |

Both modes honour every rule of §3.4 (interval, per-IP rate, honest ids, no fake replies).
"Aggressive" means scale and passive listening, not protocol abuse.

### 3.3 Node ids and virtual nodes (D54)

- **BEP 42**: id prefix = first 21 bits of `crc32c((ip & 0x030f3fff) | (r << 29))`, last id
  byte = random with low 3 bits = `r`, `r ∈ [0, 7]` (BEP 42, "Node ID restriction"; the five
  test vectors become unit tests). **For IPv4 the CRC runs over the 4 big-endian bytes** of that
  32-bit value. The BEP's prose says to "store the result in a big endian 64 bit integer and
  hash those 8 bytes", but its reference code hashes `num_octets` (4 for IPv4, 8 for IPv6) and
  its test vectors only match 4 bytes: recomputed 2026-10-26 (review), all five vectors match
  with 4 bytes (`124.31.75.21`, r 1 → `5fbfb8` masked) and none with 8 (`9bf040`). Follow the
  code and vectors, not the prose. The BEP allows "more than one node ID per external IP"
  through `r`: there are **8 valid prefixes per IPv4 address** (the other 139 bits are free), so
  a ninth virtual node would share a prefix (and keyspace region) with another one;
  `MAGNETAR_DHT_NODES` is capped at 8 for that reason. With 4 nodes we pick the 4 `r` values
  whose prefixes are farthest apart, so the passive nodes cover distant parts of the keyspace.
- External IP: UPnP `GetExternalIPAddress` when available, else a vote over the BEP 42 `ip` field
  of responses (≥ 8 agreeing responders from ≥ 4 distinct /16s). Ids are regenerated only when
  the external IP changes; ids, the external IP and the port are persisted in `settings`
  (`dht.identity`, JSON). Until an IP is known the node uses a random id and `ro=1`.
- Each virtual node is a separate UDP socket on its own port: queries carry no recipient id, so
  a remote node identifies "us" by (IP, port). Remote libtorrent nodes keep one routing slot per
  IP (`restrict_routing_ips`, dht-crawling.md §1.2), so more than a few virtual nodes add little.
- `v` = `MG` + 2 version bytes (`MG` is unregistered in BEP 20's list, checked 2026-10-26). BEP 9
  peer id: Azureus style `-MG0300-` + 12 random bytes.

### 3.4 Politeness and protocol rules (D54, D55)

1. **BEP 51 `interval` is honoured** (next sample ≥ `interval` later; `interval` clamped to
   [300, 21600] s, 0 treated as 300 so a node cannot make us hot-loop). Evidence that ignoring it
   gains nothing: libtorrent (96–99 % of BEP 51 supporters, dht-crawling.md §3.1) reuses its
   cached sample until `interval` elapses whenever it holds ≥ 20 infohashes
   (`src/kademlia/dht_storage.cpp:586-588`, branch `RC_2_0`, re-read 2026-10-26), and BEP 51
   itself says the precomputed set "will not change within that interval". bitmagnet's 60 s
   re-sample (`sample_infohashes.go:62-66`) is the anti-pattern. Throughput comes from sampling
   **more nodes**, which BEP 51's rationale describes ("survey the entire DHT within a few hours").
2. A node answering without `samples` (BEP 51: supporters "should always include the `samples`
   field") or with error 204 is marked `bep51 = no`, kept for routing, re-probed after 24 h.
   Never evicted for that (bitmagnet bug, bitmagnet.md §2.4).
3. ≤ 1 query/s per remote IP across all virtual nodes (bucket burst 2): libtorrent bans senders
   above 5 packets/s per IP for 5 min (dht-crawling.md §1.2).
4. Global outbound token bucket `MAGNETAR_DHT_QPS` over every KRPC query (sample, find_node,
   get_peers, ping). Responses we send have their own cap (300/s, 5/s per IP; excess dropped
   silently).
5. **Adaptive brake**: if the answered share over the last 60 s falls below half of its 10-min
   average while > 50 q/s, halve the rate for 5 min (a full router conntrack table or an ISP
   limiter shows up as silence, not errors). Logged and counted in stats.
6. Responses matched on `(transaction id, source address)` (bitmagnet only checks the tid,
   bitmagnet.md §2.6). Transaction ids 2 random bytes per query.
7. No forged ids, no neighbour trick, no fake `values` or `nodes` (dht-crawling.md §2).
   We **never send `announce_peer`** (we are not a peer of anything).
8. Tokens for `get_peers` answers: `sha1(secret ‖ ip)` first 8 bytes, secret rotated every 5 min,
   current and previous accepted (the BEP 5 reference scheme: "a secret that changes every five
   minutes and tokens up to ten minutes old are accepted").
9. Incoming queries with `ro=1` are answered but the sender is not put in routing tables
   (BEP 43).
10. Node farms: at most 2 pool entries per IP; ids sharing ≥ 64 bits with a target and IPs
    answering on ≥ 3 ports in one walk are not walked through (D45's rule, reused).

### 3.5 Routing table, bootstrap, pool

- Per virtual node a BEP 5 routing table: K = 8, split only around our id, good = answered
  within 15 min, questionable after, bad after 2 failures; bucket refresh after 15 min with a
  `find_node` to a random id in the bucket. Serves `find_node`/`get_peers` answers.
- Bootstrap: `core::liveness::dht::BOOTSTRAP` routers + the last 1000 good nodes persisted in
  `settings.dht.nodes` every 10 min (BEP 5: "The routing table should be saved between
  invocations"). Then `find_node` towards each own id.
- **Crawl pool** (separate from routing tables): nodes learnt from any response's
  `nodes`/sample responses and (aggressive) from incoming queries, keyed by (IP, port): id,
  `bep51` (unknown/yes/no), `next_sample_at`, `last_answer`, `failures`. Bounded LRU
  (`MAGNETAR_DHT_POOL`, 1 M aggressive ≈ 100 MB at ~100 B/entry; 200 k polite), evicting
  nodes with failures first. A node silent 3 times in a row is dropped.
- Sampler: a due queue ordered by `next_sample_at`; each send goes to the oldest due node with a
  random `target` (BEP 51: "adjusting the target value for each RPC" traverses the keyspace with
  one RPC per node). Returned `nodes` enter the pool as due now. When the due queue is empty,
  `find_node` to random targets grows the pool. Each answer yields up to 20 samples
  (libtorrent's max, `dht_storage.cpp:183`).
- **Backpressure (D62, stage 1)**: the sampler only uses a *share* of the QPS budget, set by the
  server: 0 (paused) while ≥ 10 000 hashes (or half the queue cap) wait for peer collection,
  back to 1 at half of that. Measured reason (§13): unthrottled, sampling found 62 k new
  hashes/min while peer collection managed ~2 hashes/s; the budget is worth more on lookups.
- **Lookup seeds (D61, stage 1)**: the routing tables plus the crawl pool's closest entries,
  through a 16-bit id-prefix index on the pool (exact top-k: buckets sharing the longest prefix
  are scanned first). Seeding from the routing tables alone (~100 contacts, nearly all far from
  a random infohash) sent every concurrent lookup to the same few nodes and into the per-IP
  limit.

### 3.6 Passive capture (aggressive only)

- Incoming `announce_peer` (valid token): the announcer is a peer that **has** the torrent →
  infohash enters triage with the peer `(source IP, implied_port ? source port : port)` attached,
  top priority. We also store it in the peer store (BEP 5: "the queried node should store the IP
  address … under the infohash"), 45 min TTL, 100 k entries, and serve it back honestly.
- Incoming `get_peers`: the infohash enters triage (priority 1, no peer). We answer with our
  closest `nodes` and a token, and `values` only from the peer store.
- Incoming `sample_infohashes`: answered from the peer store (≤ 20 random hashes, `interval`
  3600, `num` = store size), as a well-behaved supporter.

### 3.7 Triage, dedupe and the fetch queue (D55)

```
samples / announces / get_peers ──► seen filter (2 × Bloom) ──► batch (1000 hashes or 1 s)
   ──► Postgres triage: blocked_infohashes? torrent_infohashes? dht_seen (retry not due)?
   ──► unknown: INSERT dht_seen(status 0) ──► fetch queue (priority, bounded) ──► fetchers
```

- **Seen filter**: two Bloom generations of 96 Mbit each (12 MiB, k = 7, ~1 % false positives at
  10 M entries), rotated every 6 h (= the BEP 51 interval: a hash re-sampled after that is
  re-triaged, which lets failed fetches retry). A false positive only skips one sighting.
- **Postgres triage** decides in one round trip per batch: blocked → drop; in `torrent_infohashes`
  → drop (Nostr or already stored; `MAGNETAR_DHT_FETCH_KNOWN=true` would fetch Nostr-only ones
  to verify their file lists, off by default); in `dht_seen` with `next_attempt_at` in the future
  or NULL → drop, except an `announce` with a peer may retry a `failed` row whose last attempt is
  > 10 min old; otherwise insert/refresh `dht_seen` as `queued` with `next_attempt_at = now +
  1 h` (a lease) and enqueue. The lease is what makes the overflow and crash cases below
  recover: a `queued` row whose fetch never ran comes due again in the retry loop. A
  `queued` row with `next_attempt_at = NULL` would be dropped by triage forever.
- **Fetch queue**: in memory, bounded 100 k, three priorities (2 announce-with-peer, 1 sampled,
  0 incoming get_peers and retries), each lane served **newest first** (D60: a fresh sample's
  swarm and sampling node are the likeliest to still be there); overflow drops the
  lowest-priority oldest entry (the hash stays `queued` in `dht_seen` and is retried by the slow
  retry loop).
- **Stage 1 consumer**: peer collection (64 workers aggressive, 8 polite,
  `MAGNETAR_DHT_PEER_LOOKUPS`): announcer, else `get_peers` to the sampling node, else an
  iterative lookup (≤ 10 s, stops at 12 peers). No peer at all → `failed` / `no_peers` with the
  backoff below. Hashes with peers keep their lease until stage 2's fetcher consumes them.
- **Retry loop**: every minute, ≤ 1000 `dht_seen` rows with `next_attempt_at <= now` re-enter at
  priority 0. Backoff after a failed fetch: 1 h, 6 h, 24 h, then `next_attempt_at = NULL` (4
  attempts). `invalid`, `private`, `v2_only`, `oversize`, `alias` are final. Measured size:
  ~160 B per `dht_seen` row with its indexes (§13), not the ~70 B estimated in §10.

### 3.8 Hostile input and memory bounds (review, 2026-10-26)

Every byte from the network is attacker-controlled. Rules for stages 1, 2 and 4, each covered
by a unit test (and, for the decoders, a `proptest`/fuzz-style "arbitrary bytes never panic"
test):

- **No panics on wire data**: no indexing, slicing, `unwrap` or arithmetic on lengths from the
  wire without a check (`checked_*`/`get`). Compact `nodes` must be a multiple of 26 bytes,
  `values` entries exactly 6, `samples` a multiple of 20 (BEP 51: "N × 20 bytes"), ids and
  infohashes exactly 20, `t` 1–8 bytes, ports ≠ 0; anything else drops the message, not the
  node. At most 20 `samples` and 16 `nodes` entries are used per response, ≤ 50 `values`.
- **UDP datagrams** are read into a 2 KiB buffer; longer ones (truncated read) are dropped.
  `core::liveness::bencode` already bounds nesting (64) and string lengths against the input.
- **No traffic to non-global addresses** (SSRF): a hostile node can return `nodes`/`values`
  pointing at `127.0.0.1:5432` (Postgres), the router's admin page or other LAN hosts, and the
  sampler or the metadata fetcher would then spray them. Before any UDP send or TCP connect,
  addresses in 0/8, 10/8, 100.64/10, 127/8, 169.254/16, 172.16/12, 192.0.0/24, 192.168/16,
  198.18/15, 224/4, 240/4 and our own external address are dropped (the bootstrap routers are
  resolved names and pass the same check). Counted in stats, never logged per address.
- **Reflection**: our responses are at most a few hundred bytes larger than the query; the
  response caps of §3.4 rule 4 (300/s, 5/s per IP) bound what a spoofed-source flood can
  reflect.
- **Bounded maps**, all with a hard cap and drop-on-full: outstanding transactions (cap
  `QPS × 4 s × 2`, oldest expired first), per-remote-IP limiter buckets (LRU, 65 536 IPs, idle
  buckets are full and can be forgotten), external-IP votes (64 entries), the peer store
  (100 k), the pool, the triage channel (bounded `mpsc`, 10 000 hashes; full → the sighting is
  dropped and counted), the fetch queue (100 k), the persist channel (2 batches).
- **Metadata decode memory**: a 16 MiB info dict with a million tiny paths decodes to a
  `Value` tree several times its size. Decoding runs under a semaphore (4 at once) and the
  in-flight budget of §4 step 7 reserves `4 × metadata_size` per decode. Info dicts with
  duplicate keys at any level are `invalid` (§4 step 6).

## 4. Metadata fetch (D56, stage 2)

Own implementation of BEP 3 + BEP 10 + BEP 9 over tokio `TcpStream`, written from the BEPs
(not from bitmagnet/magnetico, AGPL; not from anacrolix, MPL file-level copyleft that we would
rather not mix in; `librqbit-peer-protocol` would pull `librqbit-core` → `sha1w` whose aws-lc
mapping is UNVERIFIED (dht-crawling.md §6.2), and the wire format is ~150 LOC).

1. **Peers**, in order: the announcer (passive); `get_peers` to the node that returned the hash
   in its sample (it stores peers for exactly those hashes, an idea from bitmagnet's pipeline);
   then an iterative `get_peers` lookup (≤ 10 s, from the pool's closest nodes). ≤ 12 peers tried
   per hash, ≤ 4 at once, per-hash budget 30 s.
2. **Handshake** (BEP 3): `19`, `"BitTorrent protocol"`, 8 reserved bytes with the extension bit
   `reserved[5] |= 0x10` (BEP 10), infohash, peer id. Reject a peer whose handshake echoes another
   infohash or lacks the extension bit.
3. **Extended handshake** (BEP 10, message id 20, ext id 0): we send
   `d1:md11:ut_metadatai1ee1:v…e` (no `metadata_size`: we have none). From the peer we need
   `m.ut_metadata` ≠ 0 and `metadata_size` in 1…`MAGNETAR_DHT_METADATA_MAX` (default 16 MiB:
   above bitmagnet's 10 MiB, below libtorrent's 30 MiB and librqbit's 32 MiB; BEP 9 sets no limit).
   Other messages (bitfield, have, PORT, …) are read and ignored; unknown ut_metadata types are
   ignored (BEP 9: "an unrecognized message ID MUST be ignored").
4. **Pieces** (BEP 9): 16 KiB blocks, all requested at once (`{msg_type:0, piece:i}`); `data`
   (type 1) carries `total_size` and the block appended after the dict; every non-last block must
   be exactly 16 384 B; `reject` (type 2) or a size mismatch fails that peer. BEP 9's own
   examples are inconsistent (reject example shows `i1e`, data example's sizes disagree,
   dht-crawling.md §1.3): code follows the prose.
5. **Verification on the raw bytes** (BEP 52: hash "the encoded form as found", never a
   decode-encode round trip): `sha1(bytes) == infohash` → v1 (or hybrid, see below). Else
   `sha256(bytes)[..20] == infohash` → the DHT carried a truncated v2 hash (BEP 52 "truncated to
   20 bytes"): if the dict also has `pieces` it is **hybrid** → stored under its v1 hash
   `sha1(bytes)` with `infohash_v2 = sha256` hex (BEP 52 "Upgrade Path"); a pure v2 dict (no
   `pieces`) has no v1 identity, which the whole data model (`InfoHash` = v1 hex) needs → status
   `v2_only`, not stored (counted). Any other mismatch → that peer fails. A hybrid's SHA-1
   was never triaged, so before it is stored it goes through the same checks as a sighting
   (`blocked_infohashes`, `torrent_infohashes`); a block records **both** hashes, and the
   sighted truncated hash is kept as `alias` (§2.4).
6. **Info dict**: `private = 1` → status `private`, not stored (BEP 27: such a torrent "MUST ONLY"
   use its private tracker; a magnet without it is useless). Name: `name.utf-8` if valid UTF-8,
   else `name` (lossy UTF-8). Files: `files[].path(.utf-8)` or single `length`, or v2 `file tree`
   for hybrids; BEP 47 padding files (`attr` contains `p`, or `.pad/` paths) dropped. Empty name
   or total size 0 → `invalid`. A dict with **duplicate keys** at any level → `invalid`: our
   lenient decoder returns the first value for a key, while other clients may show a later one,
   so a creator could put a harmless `name` first and the real one second; the bytes still hash
   to the infohash. Dropped from display (padding files, the non-preferred of `name` /
   `name.utf-8`) is not dropped from the blocklist input (§7.2).
7. **Bounds**: connect 3 s, handshake + extended handshake 5 s, whole peer 10 s; global
   in-flight bytes budget 256 MiB (a fetch reserves `metadata_size` before requesting); global
   new-TCP-connection rate ≤ 25/s (home router conntrack, dht-crawling.md §5); TCP only for now.
   **uTP later**: the share of uTP-only peers is unmeasured (UNVERIFIED); `librqbit-utp` 0.7.0
   is already in the lockfile and is the first candidate when a live run shows misses from it.

We never accept inbound TCP: we never announce, so no peer has a reason to connect to us.

## 5. Trust treatment and display of DHT results (D51, stage 5)

**Rule**: an infohash with a `dht_torrents` row **always passes `min_trust`**. In the index,
`eligible_trust = max(best publisher trust, TRUSTED (1000))` when the document has a DHT row.

Why that is sound and not a loophole: `trust` scores a **publisher's** claims (title, files,
description of an infohash it chose). A DHT row has no publisher; its name and file list come from
the info dictionary whose SHA-1 **is** the infohash, so they cannot be forged for that hash, and
the hash was announced by a live swarm within the last ~45 min (libtorrent expiry, dht-crawling.md
§1.2). What remains possible (a misleading name chosen by the torrent's creator) is the same for
every DHT indexer and is handled by the classifier and blocklist, not by WoT. This satisfies the
user's "mixed equally, not hidden by min_trust".

- **Canonical display** (D6 unchanged among events): a Nostr event when the infohash has one
  (the view gives DHT rows `trust = -1`, so `ORDER BY trust DESC` puts them last); the DHT row
  only when there is no Nostr event or every Nostr event is from a blocked publisher (a block hides
  a publisher's events, not a torrent that exists independently on the DHT). When the canonical
  event is below `min_trust` but the DHT row lets the document pass, the result still shows the
  canonical event — the same precedent as a locally trusted second publisher (docs/SEARCH.md
  "Differences from the SQL search").
- **SQL fallback** (`search.rs::push_hits_cte`, D11) is row-level and needs the same rule
  written out, or it would hide every DHT row (`trust = -1` fails `t.trust >= min_trust` for
  any threshold, 0 included): the eligibility predicate becomes `(t.origin = 'dht' OR t.trust
  >= $min OR <trusted override>)`; `publisher_count` counts `DISTINCT pubkey FILTER (WHERE
  origin = 'nostr')` (the DHT row's `''` is not a publisher); the pg_trgm typo arm (`$text <%
  lower(t.title)`) is guarded by `t.origin = 'nostr'`. That guard matters for speed:
  `dht_torrents` has no trigram index, and an unguarded `OR` arm would make the planner scan
  every DHT row on each fallback query; with the constant `origin` of each `UNION ALL` branch
  the guard folds to `false` in the DHT branch and only the GIN `search_tsv` index is used
  there (stage 5 checks this with `EXPLAIN` on a test database). One accepted difference: the
  fallback filters rows before picking the canonical one, so when every Nostr event is below
  `min_trust` it shows the DHT row, where the index shows the Nostr event.
- **Index** (`SCHEMA_VERSION` 3): facet `f_origin` (`nostr`, `dht`, both values when both), stored
  `origin`. `SearchQuery.origins: Vec<String>` (serde default, any-of; empty = all) — an optional
  filter chip, not a default.
- **API types** (all `#[serde(default)]`, so old apps and old servers interoperate):
  - `TorrentSummary.origin: Origin { Nostr (default), Dht, Both }`. DHT-only: `event_id`,
    `pubkey`, `npub` = `""`, `trust` 0, `pow_bits` 0, `publisher_count` 0.
  - `TorrentDetail.dht: Option<DhtInfo { first_seen, fetched_at, title, file_count, total_size,
    peers_seen, infohash_v2 }>`; for a DHT-only detail `events` is empty, `description` empty,
    `trackers` empty (the magnet gets `magnet::DEFAULT_TRACKERS`, as for tracker-less Nostr
    torrents), `trust` = `TrustScore { pubkey: "", score: 0, factors: [{code: "dht", delta: 0,
    detail: "Found on the DHT: name and files verified against the infohash; no publisher"}] }`.
  - `Health.features` gains `dht` when the crawler runs.
- **App**: a "DHT" badge (outlined, neutral colour) **in place of** the trust star for `Dht`;
  `Both` shows the trust star plus a small "DHT" chip; detail screen "Source: DHT (verified
  metadata)" with first-seen date; filter sheet chip "Source: Nostr / DHT"; Stats gains an
  origin breakdown and a crawler card (§10). Old apps see DHT-only results with an empty npub and a
  grey (unknown) trust badge — readable, not broken.

## 6. Classification and adult tagging (D52, stage 3)

`core::classify` (pure, no dependencies; `CLASSIFIER_VERSION` stored per row so a new version
re-classifies; the anime refinement of step 2 runs in the server, which already has
`anitomy-ng`, so the phone library stays small):

1. **Category from files** (byte balance by extension class, as bitmagnet's workflow does but
   our own lists): video / audio / book (epub, pdf, mobi, azw3, djvu, cbz, cbr…) / application
   (exe, msi, dmg, pkg, apk, deb, rpm, appimage) / picture / archive-or-unknown → `Other`; the
   class holding > 50 % of bytes wins.
2. Refinements: `Game` when the name carries a repack/scene game tag (curated list) or the files
   are an installer set typical of game repacks; `Anime` when video and the name has the fansub
   shape `[Group] Title - 05 [1080p]` (checked with the anitomy-ng parser the server already uses
   for D41); otherwise `Video`. Exact rules and an accuracy sample are stage 3's deliverable.
3. **Adult** → `Category::Porn`: adult keyword/studio/site list (word-boundary match on name and
   every path, after the normalisation of §7.2) or known adult release-code shapes, only with a
   video/picture majority. The list lives in `crates/core/src/classify/adult.txt` (plain text:
   legal content).

**Hidden by default, for every origin.** `SearchQuery.include_adult: bool` (serde default
`false`): without it, **adult** documents are excluded — unless the query explicitly selects
the `porn` category (choosing it is consent). A document is adult when its canonical category
is `porn` **or** it has a DHT row classified `porn` (index: a stored/fast `adult` flag; SQL
fallback: an `EXISTS` on `dht_torrents`). The canonical category alone would let a Nostr event
filed under `video` un-hide a torrent the classifier tagged from its own files; the opposite
direction (a Nostr publisher filing a non-adult hash under `porn`) only hides that hash, as
the canonical-category rule already would. Applied identically by the
index collector, the SQL fallback and the phone's SQLite search; saved searches carry it.

This also hides the **36 809 Nostr infohashes** categorised `porn` by their publishers (§1)
from default results. Chosen over hiding only DHT adult content: one rule matches the user's
words, and a "Show adult content" switch that only half worked would be confusing. Older apps
send no `include_adult`, so they get the new default too (documented in the release notes).
App: Settings → "Show adult content" (off), stored with the app's search preferences.

## 7. Hard illegal-content blocklist (D53, stage 3)

### 7.1 Where it applies — always, never user-overridable

- **DHT**: after the info dict is verified and before anything is written or logged. A match
  writes only `blocked_infohashes(infohash, blocked_at, rule_version, 'dht')`; the name and files
  live only in the fetcher's memory and are dropped. The hash is kept so it is never fetched again.
- **Nostr ingest** (`core::ingest`, so the phone applies it too): a kind 2003 whose title, file
  names or infohash matches is rejected with the new `Rejection::Blocked` (relay `OK false
  "blocked: content policy"`); the raw event is not stored; the server records the infohash with
  origin `nostr`. A later event for a blocked infohash is rejected by `put_torrent`. The
  **hard** tier also runs over the description and hashtags (shown in the app, so a harmless
  title cannot carry such text past it); the context tier stays on title + files, where the
  production false-positive count was measured. The rejected event id is remembered (id only,
  `blocked_events(id, blocked_at)`), so NIP-77 reconciliation does not fetch and reject the
  same event on every sync.
- **Sweep**: when `meta.dht_blocklist_version` < the built-in version, `serve` scans
  `torrent_rows` (title + files_text with both tiers, description for the hard tier, chunks of
  2000), deletes matching `events` (cascading
  `torrents`; their comments are deleted too) and `dht_torrents`, and records the hashes with
  origin `sweep`. The search index follows through the `search_changes` triggers. Deleting is
  not erasing: the old text stays in dead Postgres tuples (heap and TOAST) until vacuum and in
  Tantivy segments until they are merged. A sweep that removed anything therefore runs `VACUUM`
  on `events`, `torrents` and `dht_torrents` and forces an index merge with garbage collection
  of the old segment files (WAL, backups and dumps are out of reach and documented as such).
- No setting, env var or API turns any of this off. `MAGNETAR_BLOCKLIST_FILE` can only **add**
  terms. If the built-in list or that file fails to load, the crawler and ingest refuse to start
  (fail closed).
- Logs and stats carry counts and infohashes only, never names. That includes the code that
  runs **before** the blocklist decision: the fetcher, the info-dict parser, `core::classify`
  and `core::release` must not log names or paths at any level (a `debug!` with the name in the
  fetcher would leak exactly what is about to be blocked); a test with a capturing `tracing`
  subscriber feeds a blocked fixture through the whole pipeline at `TRACE` and asserts the
  fixture's terms never appear. The `dht --dry-run` report likewise shows names only of
  torrents that passed the blocklist.
- Order on the DHT path: verify hash → parse info dict → classify (pure, needed by the context
  tier) → blocklist → only then anything that writes, logs a name or leaves the process.

### 7.2 Matching

Input: **every** name and path variant in the info dict, not only the ones displayed: `name`
and `name.utf-8`, every `path` and `path.utf-8`, v2 `file tree` keys, padding-file paths
included (for Nostr: title, file names, and for the hard tier description and hashtags).
Normalisation: Unicode lowercase, Latin diacritics folded,
everything not a letter/digit → space, plus a second variant with common digit/symbol
substitutions undone (0→o, 1→i, 3→e, 4→a, 5→s, @→a). Tokens and token 2-/3-grams are hashed
(`sha256("magnetar-blocklist-v1:" + ngram)`) and looked up in two sets:

- **hard** terms block on their own;
- **context** terms block only when the torrent is adult (classifier result `porn` or a Nostr
  `porn` category), plus a structural rule: an age number 1–15 next to an age marker in an adult
  torrent.

Measured reason for the tiers: bitmagnet's `keywords.banned` (18 entries,
`classifier.core.yml:249-267` at `e31b30d`, MIT) used as plain word-boundary patterns over the
production titles + file lists matched **5 infohashes, none of them in `porn`** (audio 2, video 2,
application 1), all from two short terms (3 and 8 letters; read-only count by rule index, the
names were deliberately not viewed — whether they are false positives is UNVERIFIED but the
categories make it likely). Those terms go to the context tier.

**Where the list lives**: `crates/core/src/blocklist/terms.v1`, one line per entry
`hard|context <sha256 hex>`, generated by `scripts/blocklist-hash.sh` from a plain-text source
that is **not committed** (the repo carries no readable list of such terms). Initial source:
bitmagnet's `keywords.banned` (credited in the file header), its regex entries rewritten as token
sequences, the two short terms moved to the context tier. Limits (documented, UNVERIFIED recall):
hashed n-grams cannot match words glued together without separators; context terms only
fire when the classifier (or the Nostr category) says adult, so a torrent the classifier misses
is checked against the hard tier only; public CSAM hash lists
(IWF etc.) cover files, not torrent names, and are not available to us.

## 8. Ops: configuration (D58)

Server variables. **Built in stage 1**: `MAGNETAR_DHT`, `_PORT`, `_NODES`, `_QPS`, `_POOL`,
`_FETCH_KNOWN`, `_PEER_LOOKUPS`; the others are still planned (their stage in brackets).

| Variable | Default | Notes |
|---|---|---|
| `MAGNETAR_DHT` | `off` | `off` \| `polite` \| `aggressive` (§3.2) |
| `MAGNETAR_DHT_PORT` | `6891` | first UDP port; virtual node *i* listens on `PORT + i` (aggressive). Polite uses an ephemeral port |
| `MAGNETAR_DHT_NODES` | `4` (aggressive), `1` (polite) | 1–8 (BEP 42 allows 8 ids per IP) |
| `MAGNETAR_DHT_QPS` | `200` / `50` | global outbound KRPC queries per second |
| `MAGNETAR_DHT_POOL` | `1000000` / `200000` | crawl pool size (nodes) |
| `MAGNETAR_DHT_PEER_LOOKUPS` | `64` / `8` | peer collections (sampling node, then lookup) at once (D62) |
| `MAGNETAR_DHT_FETCH_CONCURRENCY` | `128` / `16` | metadata fetches in flight (stage 2) |
| `MAGNETAR_DHT_METADATA_MAX` | `16777216` | bytes, per info dict (stage 2) |
| `MAGNETAR_DHT_FETCH_KNOWN` | `false` | also fetch metadata of Nostr-only infohashes |
| `MAGNETAR_DHT_UPNP` | `true` in aggressive | map the UDP ports on the router (§9) (stage 6) |
| `MAGNETAR_DHT_UPNP_GATEWAY` | default gateway from `/proc/net/route` | `host[:port]` for a unicast SSDP M-SEARCH (needed in Docker, §9) |
| `MAGNETAR_DHT_UPNP_LOCAL_IP` | the address the SOAP connection leaves from | LAN address to map to; **required in Docker** (the container sees its bridge address) |
| `MAGNETAR_BLOCKLIST_FILE` | unset | extra plain-text terms, one per line, `context:` prefix for the context tier; additive only (stage 3) |

CLI (built in stage 1): `magnetar-server dht [--seconds N] [--mode polite|aggressive] [--dry-run]`
runs the crawler for a bounded time (default 300 s; Ctrl-C/SIGTERM stop it early, cleanly) and
prints a JSON report on stdout: sightings, unique hashes/min, triage outcome, queue, peers per
hash, the engine counters, CPU seconds and RSS. `--mode` defaults to `MAGNETAR_DHT`, or polite
when that is off. `--dry-run`: no database at all (dedupe in memory). `serve` starts it when
`MAGNETAR_DHT` ≠ `off`, after migrations (the blocklist sweep joins in stage 3); it stops within
`serve`'s 10 s daemon grace, saving the identity and 1000 contacts.

## 9. UPnP, ports, Docker (D57, stage 6)

- Crate: **`igd-next` 0.18.0** (MIT, 2026-10-04, ~5 M recent downloads; deps hyper 1 / xmltree /
  xml-rs / url, all MIT, hyper already in the tree via reqwest), `default-features = false,
  features = ["aio_tokio"]`. Source read in `~/.cache/magnetar-dht-scratch/crates/igd`:
  `aio::tokio::search_gateway(SearchOptions)`, `Gateway::{get_external_ip, add_port(protocol,
  external_port, local_addr, lease_duration, description), remove_port}`.
  `SearchOptions.broadcast_address` is settable, which gives us both the unicast M-SEARCH the
  neufbox needs (§1) and a **test seam**: tests point it at an in-process fake SSDP + SOAP
  gateway on 127.0.0.1, so no real router is touched. Rejected: `igd` 0.12.1 (2023, superseded
  fork), `rupnp` (generic UPnP, more code for the same), `easy-upnp` (WTFPL, not in the allowed
  licences), `portmapper` (n0, pulls their netwatch stack; NAT-PMP/PCP would be its only gain).
- Discovery order: unicast M-SEARCH to `MAGNETAR_DHT_UPNP_GATEWAY` (default gateway), then
  multicast. **UDP only**: one mapping per virtual node, external port = internal port,
  description `magnetar-dht`, lease 3600 s renewed every 30 min (falls back to lease 0 when the
  router answers `OnlyPermanentLeasesSupported`), removed on clean shutdown. A port already mapped
  by someone else → logged, the node runs unmapped. No TCP mapping: we never accept peer
  connections (§4).
- Reachability is observed, not assumed: stats count incoming queries per minute; zero after
  10 min with a mapping means the router or a firewall drops them.
- **Docker**: SSDP multicast does not leave the bridge network, and the container's default
  gateway is the bridge. So in Docker set `MAGNETAR_DHT_UPNP_GATEWAY=<router ip>` and
  `MAGNETAR_DHT_UPNP_LOCAL_IP=<host LAN ip>` (MiniUPnPd's secure mode only maps to the
  requester's own address, which is the host's after masquerading), and publish the UDP ports.
  Ports come from an overlay so a default `compose.yml` publishes nothing new:
  `docker/compose.dht.yml` with `ports: ["${MAGNETAR_DHT_PUBLISH:-6891-6894}:6891-6894/udp"]` and
  the DHT variables, used as `docker compose -f compose.yml -f docker/compose.dht.yml up -d`.
  Outbound UDP from the container is masqueraded with the source port kept when free (Linux
  default), so remote nodes see the same port they can reach. UNVERIFIED on the real router until
  stage 8.
- The laptop's own nftables ruleset (169 lines) may need the published UDP ports allowed; checked
  in stage 8, without changing any router setting before then.

## 10. Stats, health, resources (D58, stage 6)

- `/api/v1/stats.dht` (`DhtStats`, serde default): mode, virtual nodes (id prefix, port, mapped),
  external IP class (public / private / CGNAT, never the address itself), routing-table sizes,
  pool size and BEP 51 share, queries out/s and answered share, brake active, incoming queries/min
  (aggressive), samples/h, new unique hashes/h, queue length by priority, fetches in flight,
  fetch ok/fail per hour by error code, hit rate, stored total, blocked total, adult share,
  `v2_only`/`private` counts, UPnP state (mapped ports, lease expiry, last error).
- `stats_series` metrics: `dht_torrents_added`, `dht_hashes_seen`, `dht_fetch_ok`,
  `dht_fetch_fail`, `dht_blocked`; breakdown field `origin`.
- Targets on the laptop (to be measured in stage 8): ≤ 25 % of one core, ≤ 400 MB RSS for the
  crawler, ≤ 8 Mbit/s down, ≤ 25 new TCP connections/s, ~10 k UDP conntrack entries at
  200 q/s (30 s unreplied timeout × rate + flows to answering nodes).
- **Disk, no cap** (user decision): a DHT torrent costs about 4 KB (heap + `search_tsv` GIN +
  attrs + schedule + infohash row ≈ 3 KB, index ≈ 0.7 KB; DHT rows have no description, which is
  most of a Nostr row's 6.6 KB) — estimate, measured in stage 8. At 10–20 k torrents/h that is
  1–2 GB a day; `dht_seen` adds ~70 B per failed hash. Growth is shown in Stats (`db_size`), not
  limited.
- **Seed checks**: DHT inserts are scheduled like new Nostr ones (`due_at = -first_seen`, newest
  first). At 10–20 k/h this is close to D45's measured 29 k checks/h; stage 8 measures the backlog
  and may raise `MAGNETAR_LIVENESS_BATCH` or seed the first result from the fetch.

## 11. What is deliberately not done

IPv6 DHT (BEP 32) and uTP are later; no NAT-PMP/PCP; no `announce_peer` from us; no crawling on
the phone (battery, data; the app only displays); no re-sampling before `interval`; no keeping of
blocked names anywhere; nothing from the DHT is published to Nostr.

## 12. Stages

| Stage | Builds | Verifies |
|---|---|---|
| 0 | this document, D49–D58 | production counts, librqbit-dht API, BEP texts, router UPnP (read-only) |
| 1 | `core::dht` KRPC, BEP 42 ids, routing, pool, sampler, limiter, responder, peer store; **pulled forward from stage 4** (swarm plan): migration §2.3, `server::dht` triage, `dht_seen` queue, retry, peer collection, `dht` CLI, config, `serve` daemon | BEP 5/42/51 vectors (BEP 42 over 4 bytes), arbitrary-bytes no-panic decoders, address filter, map caps (§3.8), fake-network tests, Postgres tests, bounded live aggressive runs (§13) |
| 2 | `core::metainfo` BEP 3/10/9 fetcher, v1/hybrid/v2 verification | fake-peer tests (reject, short piece, wrong hash, hybrid, private, duplicate keys, LAN/loopback peer refused), bounded live fetch of sampled hashes |
| 3 | `core::classify`, `core::blocklist`, `Rejection::Blocked`, `scripts/blocklist-hash.sh` | accuracy sample, blocklist vectors (hashed, every name/path variant), Nostr ingest rejection (description/hashtags too), no-name-in-logs test at `TRACE` |
| 4 | `server::dht::persist` (dht_torrents + attrs batches, alias rows), fetch results into `dht_seen` | Postgres tests, hybrid alias, bounded live aggressive run without UPnP |
| 5 | `torrent_rows` readers, index `SCHEMA_VERSION` 3 (origin, eligible_trust, include_adult), API/ffi types, sweep | search/filters/detail tests on both paths (DHT rows pass `min_trust` in the SQL fallback too, `EXPLAIN` shows no seq scan of `dht_torrents`), `recompute_published` Nostr-only, sweep vacuum + merge |
| 6 | UPnP, `docker/compose.dht.yml`, stats/health | fake gateway tests, stats API |
| 7 | app: badge, filter chip, adult switch, crawler card | Gradle build + unit tests |
| 8 | live tuning on the laptop | rates, hit rate, resources, disk per torrent |

## 13. Stage 1: built and measured (2026-10-26)

**Code.** `crates/core/src/dht/` (krpc, id, addr, routing, pool, limiter, peerstore, engine,
lookup), `crates/server/src/dht/` (mod, seen, queue, store), migration
`20261101000000_dht.sql`, `magnetar-server dht`, `MAGNETAR_DHT*` options in `config.rs`.
New dependency: `proptest` 1.12.0 (MIT/Apache-2.0), dev only. CRC32C is in-house (D59).

**Tests.** KRPC codec against the BEP 5 example packets, BEP 51 round trip, BEP 42 `ip` and
BEP 43 `ro`, the five BEP 42 vectors (4-byte CRC) and the 8-byte prose counter-check, strict
lengths/caps; three proptest suites (arbitrary bytes, KRPC-shaped garbage, mutated valid
messages) never panic; address filter; limiter, pool (prefix index exact top-k), routing,
peer store caps. Fake DHT (`core/tests/dht_fake_network.rs`, 1–12 in-process nodes on distinct
127.0.x.y addresses): sampling of every announced hash, BEP 42 vote, peers from the sampling
node and through a lookup, passive capture, honest peer store, forged token refused, polite
read-only, 207 hostile datagrams, cancelled queries release their transactions, sampler pause
and throttle. Postgres (`server/tests/dht_pg.rs`): triggers (counts, schedule, search_changes),
the view, the Nostr delete keeping DHT-held hashes, triage (blocked/known/duplicates/lease),
backoff 1 h/6 h/24 h/final and the announce exception, the retry loop settling known and
blocked rows, identity/contacts round trip, and the whole pipeline against a 10-node fake DHT
(40 hashes: all persisted but the known one, peers for every collected hash, identity saved, a
second run queues nothing again, a stop request returns in < 8 s).

**Live runs** from the laptop (home IP, aggressive, 4 virtual nodes on UDP 6891–6894, **no
UPnP and no router port opened**, so almost no unsolicited inbound traffic), release build,
scratch database `dht_stage1_live`, 270 s each:

| | run 1 (before fixes) | run 2 (leak + seeds fixed) | run 3 (with backpressure, current) |
|---|---|---|---|
| queries/s (answered share) | 170 (54 %) | 200 (52 %) | 164 (52 %) |
| sample_infohashes / get_peers sent | 33 371 / 12 404 | 42 869 / 10 985 | 1 764 / 42 322 |
| unique infohashes (per min) | 211 950 (47 100) | 280 986 (62 441) | 11 350 (2 522 over 270 s; sampling paused 210 s by backpressure) |
| hashes given peers (per min) | 7 810 (1 736) | 630 (140) | 2 404 (534) |
| share with ≥ 1 peer | 6.7 % | 84 % | 77 % |
| peers per hash: mean / median / p90 / max | 0.7 / 0 / 0 / 63 | 8.9 / 3 / 26 / 64 | 7.9 / 2 / 22 / 60 |
| peers from the sampling node / via lookup | 113 / 7 694 | 119 / 511 | 130 / 2 273 |
| queries per collected hash | 17.9 | 18.8 | 21.3 |
| `busy` (transaction map full) / per-IP refusals | 19 324 / 116 431 | 0 / 1 123 | 0 / 9 779 |
| CPU (whole process) | 6.0 s (2.2 % of a core) | 5.0 s (1.9 %) | 4.2 s (1.6 %) |
| peak RSS | 68 MB | 75 MB | 47 MB |
| crawl pool at the end (BEP 51 yes / no / unknown) | 129 k (11 k / 6 k / 111 k) | 149 k (15 k / 8 k / 126 k) | 60 k (0.6 k / 0.5 k / 59 k) |

Run 1 exposed two bugs (fixed, D61): leaked transactions filled the pending map (`busy`), and
lookups seeded only from the routing tables hit the per-IP limit, so most hashes got no peers.
Run 2 showed discovery outrunning collection ~450:1, hence D62. Run 3: the queue reaches
10 000 in about a minute, then the budget goes to lookups. Incoming queries without an open
port: 11 ping, 264 find_node, 109 get_peers, 4 announce_peer in 270 s (conntrack-allowed flows
only). `dht_seen` after run 3: 10 798 `queued`, 552 `failed` (`no_peers`), 1.8 MB ≈ 160 B per
row. A 60 s restart on the same database loaded 1000 saved contacts and the saved identity, and
76 of 10 706 sighted hashes were already in `dht_seen` (not queued again). SIGINT after 10 s:
report printed at 9.9 s, ports released.

**Not verified in stage 1.** Inbound reachability and passive capture at scale (needs the UPnP
stage or an opened port; the user's router was not touched); polite mode live; behaviour past
5 minutes (pool growth to 1 M, Bloom rotation at 6 h, memory at that size, the retry loop on
real expired leases, the brake on a real conntrack limit); why only ~5 % of hashes get peers
from the node that sampled them (a node behind NAT that stopped answering, or a lookup ran
first because the per-IP bucket was spent — not measured); whether 64 collectors use the full
budget (run 3 averaged 164 of 200 q/s, waiting on 4 s timeouts).
