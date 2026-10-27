# DHT crawler (ARCHITECTURE.md D49–D75, D93, D95)

Design document for the native Mainline DHT crawler. Status: **stages 1 (discovery), 2
(metadata fetch) and 3 (classification and safety) built**: KRPC node, BEP 42 ids, routing,
crawl pool, BEP 51 sampler, passive capture, peer collection, the migration of §2.3, triage,
the persisted `dht_seen` queue, the `dht` CLI and the `serve` daemon (§13), the BEP 3/10/9
fetcher, fetch workers and the `dht_torrents` writer behind a fail-closed content screen (§14),
then `core::classify`, the hard blocklist `core::blocklist` installed as that screen and on
Nostr ingest, the adult filter on every search path, and the enrichers / attrs backfill on DHT
rows (§15), then **product integration and ops** (§16): DHT torrents in the search index,
facets, SQL fallback, detail and API (`origin`, D73), the crawler's live stats and health
fields (D74), UPnP port mapping and the Docker overlay (D72), the app's source badge, Source
chips, Stats and Status (D74), and a 10-minute live run on a production copy (D75). The final
integration check (§17) found nothing to fix.
Each section says which stage builds it (§12); later stages update this file as they land.

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
| `enrich/mod.rs` `due`, `load_groups`, `coverage` | IMDb/TMDB matching by name works on DHT names (D40 already uses the largest video file). **Done in stage 3** (D70): `due` walks `torrent_infohashes` |
| `enrich_anime.rs` pending query | anime by name (`category = 'anime'`, §6). **Done in stage 3** |
| `attrs.rs` backfill | re-parse DHT names when `PARSER_VERSION` moves. **Done in stage 3** (Nostr event first, else the DHT row) |
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
                       store.rs (triage, leases, identity) — stage 1; persist.rs (results →
                       Postgres), screen.rs (content screen seam, D65) — stage 2; upnp, stats (6)
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
  of responses (≥ 8 agreeing responders from ≥ 4 distinct /16s; once an address is known, a
  new one must also have more responders than the known one in the same 10-minute window, D63).
  Ids are regenerated only when
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
   **Second trigger, the router health probe (D66)**: a TCP connect every 5 s to
   `MAGNETAR_DHT_PROBE` (1.1.1.1:443, 9.9.9.9:443; ICMP is blocked on the user's line and
   rate-limited by the gateway). Saturated when 3 of the last 6 probes fail or their median
   exceeds `max(3 × baseline, baseline + 40 ms)` (baseline = 10th percentile of the last 10 min);
   then KRPC queries **and** TCP connection attempts are halved for 5 min, extended while it
   lasts. Benchmark: safe rates kept 17–19 ms, saturation gave 122 ms or 100 % loss.
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
  Since D95 (§19) the seeds are ranked by responsiveness among the 64 closest pool contacts,
  and pool nodes that never answered two queries are dropped and skipped.

### 3.6 Passive capture (aggressive only)

- Incoming `announce_peer` (valid token): the announcer is a peer that **has** the torrent →
  infohash enters triage with the peer `(source IP, implied_port ? source port : port)` attached,
  top priority. The announcer's own `get_peers` usually came first, so the seen filter already
  knows the hash: an announce with a peer bypasses it (§18, D93) and moves the queued hash to
  the announce lane with that peer, hands the peer to a collection in flight, or goes to
  Postgres triage again (which retries a `failed` row whose last attempt is > 10 min old). We also store it in the peer store (BEP 5: "the queried node should store the IP
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
- **Consumers**: peer collection (64 workers aggressive, 8 polite,
  `MAGNETAR_DHT_PEER_LOOKUPS`): announcer, else `get_peers` to the sampling node, else an
  iterative lookup (≤ 10 s, stops at 24 peers, `MAGNETAR_DHT_PEERS_PER_HASH`; a node silent for
  1.5 s no longer holds up the walk, §18). No peer at all → `failed` / `no_peers` with the
  backoff below, written by a status writer in batches (≤ 500 rows or 1 s, one statement), so a
  collector never waits on the database (§18). Hashes with peers go through a bounded channel (capacity = fetch workers, so
  collection blocks while every fetcher is busy) to `MAGNETAR_DHT_FETCH_CONCURRENCY` fetch
  workers (stage 2); results are written by one persister in batches (§2.4, §14). A fetch cut
  short by a stop keeps its lease and comes back through the retry loop.
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
- **Bounded maps**, all with a hard cap: outstanding transactions (cap `QPS × 4 s × 2`; full →
  the query is refused as `busy`; a cancelled or timed-out query removes its own entry),
  per-remote-IP limiter buckets (65 536 IPs; when full, buckets that have refilled are
  forgotten, else the new IP is refused), external-IP votes (64 candidates, the weakest evicted
  for a new one, restarted every 10 min, D63), the peer store (100 k), the pool, the triage
  channel (bounded `mpsc`, 10 000 hashes; full → the sighting is dropped and counted), the
  fetch queue (100 k).
- **Seen filter keys**: Bloom bit positions come from SipHash under per-process random keys,
  so a node cannot pick a look-alike infohash that shadows a real one (D63).
- **Metadata decode memory**: a 16 MiB info dict with a million tiny paths decodes to a
  `Value` tree tens of times its size (2-byte values, ≥ 32 B each decoded). Decoding runs under
  a semaphore (4 at once); before it runs, the verified bytes are scanned without allocating
  to count values: more than `metadata_max / 8` = `invalid` (`too_complex`), else the decode
  reserves `3 × size + 64 B × values` from a decode budget of its own (D67). Info dicts with
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
   new-TCP-connection **attempts ≤ 150/s** and **≤ 350 connections open** (D66, measured on the
   user's router: [research/router-bench.md](research/router-bench.md) — 200 attempts/s safe,
   400 breaks; ~440 open held; the 25/s first planned here was ~6× too low and starved the
   fetcher, §14); TCP only for now.
   **uTP later**: the share of uTP-only peers is unmeasured (UNVERIFIED); `librqbit-utp` 0.7.0
   is already in the lockfile and is the first candidate when a live run shows misses from it.

We never accept inbound TCP: we never announce, so no peer has a reason to connect to us.

**As built (stage 2, D64)**, refinements of the steps above: our extended handshake is sent
only after the peer's handshake showed the BEP 10 bit; a `request` from the peer is answered
with `reject` (BEP 9 MUST, ≤ 64 per session); other messages, and `extended` messages we do
not use, are skipped within 4 MiB per peer and an `extended` message above 64 KiB ends the
peer; decode memory (priced per dict, D67) comes from a budget of its own, so downloads
holding the 256 MiB in-flight budget cannot starve the decodes; an announced hash is fetched from the announcer only. After step 6 the
order is (§7.1): **screen** (D65) → store or block. Stage 2 has no classifier: rows are stored
with `category = other`, `classifier_version = 0`, which stage 3 re-classifies.

## 5. Trust treatment and display of DHT results (D51, as built in stage 4: D73, §16)

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
- **Index** (`SCHEMA_VERSION` 4; version 3 is the stage 3 `adult` flag, D70): facet `f_origin` (`nostr`, `dht`, both values when both), stored
  `origin`. `SearchQuery.origins: Vec<String>` (serde default, any-of; empty = all) — an optional
  filter chip, not a default.
- **Empty-query browse** (D76, user choice 2026-10-26, after the live crawl flooded the default
  list): with no text and no source filter, a DHT-only torrent is listed once a seed check
  found peers (> 0); Nostr results unchanged; any text search or the DHT chip returns every DHT
  result (docs/SEARCH.md "Query").
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

## 6. Classification and adult tagging (D52, stage 3; as built: D69, D70, §15)

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

## 7. Hard illegal-content blocklist (D53, stage 3; as built: D68, §15)

**Binding user rule (2026-10-26):** NO agent and no code path may ever view, print, log, sample, export or "verify" the names, files or descriptions of anything the illegal-content blocklist matches — not even to check for false positives, not in tests, not in reviews, not in live runs. Count matches only by rule id / category, as stage 0 did. False positives are accepted: a blocked torrent stays blocked.

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
Normalisation: Unicode NFKC, invisible characters and leftover combining marks removed (not
treated as separators; D71), Unicode lowercase, Latin diacritics folded,
everything not a letter/digit → space, plus a second variant with common digit/symbol
substitutions and Cyrillic/Greek look-alikes undone (0→o, 1→i, 3→e, 4→a, 5→s, @→a, о→o, …),
and for term lookup only a collapsed form of each where runs of one-character tokens are
joined (`z.o.r.b` → `zorb`). Tokens and token 2-/3-grams are hashed
(`sha256("magnetar-blocklist-v1:" + ngram)`) and looked up in two sets:

- **hard** terms block on their own;
- **context** terms block only when the torrent is adult (classifier result `porn` or a Nostr
  `porn` category), plus a structural rule: an age number 1–15 next to an age marker in an adult
  torrent.

Measured reason for the tiers: bitmagnet's `keywords.banned` (18 entries,
`classifier.core.yml:249-267` at `e31b30d`, MIT) used as plain word-boundary patterns over the
production titles + file lists matched **5 infohashes, none of them in `porn`** (audio 2, video 2,
application 1), all from two short terms (3 and 8 letters; read-only count by rule index; the
names were not and will never be viewed, per the rule above). Those terms go to the context
tier; whatever they still match stays blocked (false positives are accepted).

**As built (D68)**: CJK runs are also looked up as character 2–4-grams (those scripts have no
spaces; a token-only matcher missed them in the live sample), the age rule covers ages 1–17
and an **explicit** annotation (`12 yo`, `12 years old`) blocks any torrent while ordinary-word
markers (`ans`, `aged`, …) need an adult torrent, and "adult" for the context tier is the
classifier's category `porn` **or** its adult signal whatever the file types.

**Where the list lives**: `crates/core/src/blocklist/terms.v1`, one line per entry
`hard|context <sha256 hex>`, generated by `scripts/blocklist-hash.sh` from a plain-text source
that is **not committed** (the repo carries no readable list of such terms; 52 hard and 27
context entries in version 1, ASCII words and CJK terms). Initial source:
bitmagnet's `keywords.banned` (credited in the file header), its regex entries rewritten as token
sequences, the two short terms moved to the context tier. Limits (documented, UNVERIFIED recall):
hashed n-grams cannot match words glued together without separators; context terms only
fire when the classifier (or the Nostr category) says adult, so a torrent the classifier misses
is checked against the hard tier only; public CSAM hash lists
(IWF etc.) cover files, not torrent names, and are not available to us.

## 8. Ops: configuration (D58)

Server variables. **Built in stage 1**: `MAGNETAR_DHT`, `_PORT`, `_NODES`, `_QPS`, `_POOL`,
`_FETCH_KNOWN`, `_PEER_LOOKUPS`; **stage 2**: `_FETCH_CONCURRENCY` (1–2048), `_METADATA_MAX`
(16 KiB–64 MiB), `_TCP_RATE`, `_TCP_MAX`, `_PROBE`; **stage 3**: `MAGNETAR_BLOCKLIST_FILE`; the
others are still planned (their stage in brackets).

| Variable | Default | Notes |
|---|---|---|
| `MAGNETAR_DHT` | `off` | `off` \| `polite` \| `aggressive` (§3.2) |
| `MAGNETAR_DHT_PORT` | `6891` | first UDP port; virtual node *i* listens on `PORT + i` (aggressive). Polite uses an ephemeral port |
| `MAGNETAR_DHT_NODES` | `4` (aggressive), `1` (polite) | 1–8 (BEP 42 allows 8 ids per IP) |
| `MAGNETAR_DHT_QPS` | `200` / `50` | global outbound KRPC queries per second |
| `MAGNETAR_DHT_POOL` | `1000000` / `200000` | crawl pool size (nodes) |
| `MAGNETAR_DHT_PEER_LOOKUPS` | `64` / `8` | peer collections (sampling node, then lookup) at once (D62) |
| `MAGNETAR_DHT_FETCH_CONCURRENCY` | `128` / `16` | metadata fetches in flight |
| `MAGNETAR_DHT_LOOKUP_TIMEOUT_MS` | `4000` | soft per-query timeout of lookups (100–4000, §18, D93) |
| `MAGNETAR_DHT_PEERS_PER_HASH` | `24` | peers tried per hash by the fetcher, and where a lookup stops (1–64, §18) |
| `MAGNETAR_DHT_METADATA_MAX` | `16777216` | bytes, per info dict |
| `MAGNETAR_DHT_TCP_RATE` | `150` | new TCP connection attempts/s, all fetches (D66) |
| `MAGNETAR_DHT_TCP_MAX` | `350` | TCP connections open at once (D66) |
| `MAGNETAR_DHT_PROBE` | `1.1.1.1:443,9.9.9.9:443` | health probe targets, or `off` (D66) |
| `MAGNETAR_DHT_FETCH_KNOWN` | `false` | also fetch metadata of Nostr-only infohashes |
| `MAGNETAR_DHT_UPNP` | `true` in aggressive | map the UDP ports on the router (§9) (stage 6) |
| `MAGNETAR_DHT_UPNP_GATEWAY` | default gateway from `/proc/net/route` | `host[:port]` for a unicast SSDP M-SEARCH (needed in Docker, §9) |
| `MAGNETAR_DHT_UPNP_LOCAL_IP` | the address the SOAP connection leaves from | LAN address to map to; **required in Docker** (the container sees its bridge address) |
| `MAGNETAR_BLOCKLIST_FILE` | unset | extra plain-text terms, one per line (1–3 words, or one 2–4-character CJK term), `context:` prefix for the context tier; additive only; read at the start of every command, a file that does not load stops it (stage 3) |

CLI (built in stage 1): `magnetar-server dht [--seconds N] [--mode polite|aggressive] [--dry-run]`
runs the crawler for a bounded time (default 300 s; Ctrl-C/SIGTERM stop it early, cleanly) and
prints a JSON report on stdout: sightings, unique hashes/min, triage outcome, queue, peers per
hash, the engine counters, CPU seconds and RSS. `--mode` defaults to `MAGNETAR_DHT`, or polite
when that is off. `--dry-run`: no database at all (dedupe in memory). `serve` starts it when
`MAGNETAR_DHT` ≠ `off`, after migrations (the blocklist sweep joins in stage 3); it stops within
`serve`'s 10 s daemon grace, saving the identity and 1000 contacts.

## 9. UPnP, ports, Docker (D57, as built in stage 4: D72, §16)

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
  router answers `OnlyPermanentLeasesSupported`), removed on clean shutdown (every port mapped in
  any round, removed concurrently within 2 s, so a failed renewal never strands a lease-0
  mapping). A port already mapped by someone else (718) → logged, the node runs unmapped, and
  that port is never removed at stop (it is the other host's). No TCP mapping: we never accept peer
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

## 10. Stats, health, resources (D58, as built in stage 4: D74, §16)

- `/api/v1/stats.dht` (`DhtStats`, serde default): mode, virtual nodes (id prefix, port, mapped),
  external IP class (public / private / CGNAT, never the address itself), routing-table sizes,
  pool size and BEP 51 share, queries out/s and answered share, brake active, incoming queries/min
  (aggressive), samples/h, new unique hashes/h, queue length by priority, fetches in flight,
  fetch ok/fail per hour by error code, hit rate, stored total, blocked total, adult share,
  `v2_only`/`private` counts, UPnP state (mapped ports, lease expiry, last error).
- D93 adds (serde default, older servers leave them 0): `collected`, `from_source`, `looked_up`,
  `announce_fetches`, `announce_upgraded`, `incoming_*` per query type, `fake_peers_skipped` /
  `_banned` / `_tracked`, `lookup_mean_ms`, `queries_per_lookup`, `lookup_slow_share`, and the
  answer share by node age `answered_share_young` / `_old` / `_unpooled` / `_repeat` (§18).
- D95 adds (serde default): `lookup_answered_share`, `lookup_seeds_fresh_share`,
  `queries_per_stored` (rate window), `pool_dropped_dead`, `lookup_dead_skipped` (since the
  start) (§19).
- `stats_series` metrics: `dht_torrents_added`, `dht_hashes_seen`, `dht_fetch_ok`,
  `dht_fetch_fail`, `dht_blocked`; breakdown field `origin`.
- Targets on the laptop (to be measured in stage 8): ≤ 25 % of one core, ≤ 400 MB RSS for the
  crawler, ≤ 8 Mbit/s down, ≤ 150 new TCP connections/s (D66), ~10 k UDP conntrack entries at
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
| 2 (built, §14) | `core::metainfo` BEP 3/10/9 fetcher, v1/hybrid/v2 verification; **pulled forward from stage 4**: fetch workers, `server::dht::persist` (dht_torrents + attrs batches, alias rows, final statuses), the fail-closed content-screen seam (D65) | fake-peer tests (reject, short piece, wrong hash, hybrid, private, duplicate keys, LAN/loopback peer refused), a real librqbit seed, Postgres storage tests, bounded live fetch of sampled hashes |
| 3 (built, §15) | `core::classify`, `core::blocklist`, `Rejection::Blocked`, `scripts/blocklist-hash.sh`; **pulled forward from stage 5/7**: `SearchQuery.include_adult` on the index (`SCHEMA_VERSION` 3), SQL fallback and phone, the app's adult switch; enrichers and attrs backfill on `torrent_rows` | accuracy sample, blocklist vectors (hashed, every name/path variant), Nostr ingest rejection (description/hashtags too), no-name-in-logs test at `TRACE` |
| 4 (built, §16; swarm stage "product integration and ops", covers the planned 5–7 except the sweep) | `torrent_rows` readers (index documents, detail, SQL fallback), index `SCHEMA_VERSION` 4 (origin, eligible_trust), API/ffi types, UPnP, `docker/compose.dht.yml`, stats/health, app badge, Source chips, Stats/Status | both search paths, `EXPLAIN`, fake gateway, stats API, Android unit tests, 10-min live run on a production copy |
| final (§17) | no new features | merge onto `main`, migrations from scratch and on a production copy, gates, debug APK + app unit tests, Docker image, 10-min live run on a production copy with every DHT API surface smoke-tested |
| 5 (open) | the Nostr **sweep** of §7.1 (stored events re-screened when the blocklist version moves, vacuum + index merge) | not built yet |
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
peer store caps. Fake DHT (`core/tests/it/dht_fake_network.rs`, 1–12 in-process nodes on distinct
127.0.x.y addresses): sampling of every announced hash, BEP 42 vote, peers from the sampling
node and through a lookup, passive capture, honest peer store, forged token refused, polite
read-only, 207 hostile datagrams, cancelled queries release their transactions, sampler pause
and throttle. Postgres (`server/tests/it/dht_pg.rs`): triggers (counts, schedule, search_changes),
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

**Review (2026-10-26, commit "STAGE discovery: review fixes").** Fixed: (1) the external-IP
vote kept at most 64 candidates and never forgot one, so after enough junk `ip` claims a real
address change (a home IP renewed by the ISP) could never be voted in, and stale claims added up
forever — now a windowed vote with eviction and a majority rule (D63, three unit tests);
(2) the seen filter's "keyed" positions were the hash XOR a key, so two infohashes sharing their
first 16 bytes always collided — now SipHash (D63, unit test); (3) a failed crawler start under
`serve` (bootstrap names not resolvable yet at boot, port still taken, database error) left the
crawler off until the next restart — now retried after 30 s doubling to 15 min; (4) an empty
port list started one virtual node with no socket, and the first query panicked indexing the
socket list (regression test); (5) the `/proc/self/stat` parser sliced past the end on a
malformed line. Not changed, noted: a triage batch that fails on a database error is lost until
the Bloom generation rotates (its hashes are already marked seen); a lookup that ends while its
queries wait for a pace slot wastes those slots (part of the 164-of-200 q/s gap); the stage 1
hourly re-collection described in §3.7.

## 14. Stage 2: built and measured (2026-10-26)

**Code.** `crates/core/src/metainfo/` (wire, fetch, info; feature `dht-crawl`, no new
dependency), `crates/server/src/dht/persist.rs` (fetch results → Postgres) and
`crates/server/src/dht/screen.rs` (the content-screen seam, D65); fetch workers and a batching
persister in `server::dht`; `MAGNETAR_DHT_FETCH_CONCURRENCY`, `MAGNETAR_DHT_METADATA_MAX`.
Protocol refinements are D64; the fail-closed storage rule is D65.

**Tests.** Unit: wire layout against the BEP 9/10 example messages, info dict parser (padding
files, `name.utf-8`, duplicate keys at any depth, v2 `file tree`, sizes overflowing, NUL),
v1 / hybrid / v2-only verification, proptest "arbitrary bytes never panic" on every decoder.
Fake peers (`core/tests/it/metainfo_fetch.rs`, 127.0.0.1): a good multi-block peer, and reject,
short block, wrong bytes (hash mismatch), oversize claim, zero/negative size, size off by ±1,
wrong `total_size`, no extension bit (we then send nothing after the handshake), wrong
infohash, no `ut_metadata`, silent peer, 4 GiB length prefix, 1 MiB extended message, `have`
flood, garbage stream, a peer asking us for metadata (answered with `reject`), private,
hybrid sighted by its truncated v2 hash, pure v2, duplicate keys, LAN/loopback refused in
production config, the 12-peer cap and the hash budget — every hostile case ends in < 5 s with
its error code. **Real client** (`core/tests/it/metainfo_librqbit.rs`): an in-process librqbit
9.0.1 session seeds a generated 402-file torrent (25 732-byte info dict, 2 metadata blocks) on
127.0.0.1; the fetcher verifies it in ~3 ms. Postgres (`server/tests/it/dht_fetch_pg.rs`):
storage with its triggers (infohash row with `dht`, seed-check schedule, search log, SQL
`search_tsv` match, release attributes), duplicates, the screen blocking on a padding path with
no trace of the name anywhere, no screen → nothing stored and the row re-queued, hybrid alias,
hybrid whose SHA-1 is known, final statuses and backoff, and the whole pipeline (6-node fake
DHT → peer collection → fake seeder → Postgres): 20/20 fetched, 19 stored, 1 blocked.

**Live run** (release build, home IP, aggressive, 4 virtual nodes on UDP 6891–6894, **no UPnP,
no router port opened**, no content screen so nothing named stored; scratch database
`dht_stage2_live`, 540 s, `MAGNETAR_DHT_FETCH_CONCURRENCY` 128, 25 new TCP connections/s):

| | |
|---|---|
| unique infohashes / queued | 11 175 / 11 173 (sampling paused 497 s of 540 by backpressure, D62) |
| hashes given peers (≥ 1 peer) | 4 138 collected, 3 176 with peers (77 %); mean 8.3 peers, median 2, p90 25 |
| metadata fetches finished / verified | 2 910 / **522** (2 hybrid) → **58 verified info dicts per minute**, success rate **17.9 %** of finished fetches |
| hash-level failures (`last_error`) | 1 187 timeout (30 s hash budget), 936 connect, 198 closed, 29 no metadata, 24 handshake, 13 no `ut_metadata`, 1 reject; plus 962 `no_peers` from collection |
| peer sessions | 13 890 started, 1 674 TCP connections established (12 %); 6 756 connect failures, 747 closed, 146 timeouts, 66 no `ut_metadata`, 35 no `metadata_size`, 25 bad handshake, 3 reject |
| mean time of a successful fetch | 16.4 s |
| verified metadata | 25.1 MB (≈ 48 KB per info dict) |
| bandwidth | UDP 11.1 MB in / 7.7 MB out, TCP 38.3 MB in / 0.3 MB out → 0.73 Mbit/s in, 0.12 Mbit/s out |
| KRPC | 134 queries/s, 51 % answered |
| CPU / RSS | 10.7 s (2.0 % of one core) / 67.5 MB peak |
| database after the run | `dht_torrents` 0 rows (no screen, D65); `dht_seen`: 7 303 queued, 522 `unscreened` (due again in 6 h), 2 388 failed |

**Reading.** The fetch rate is bound by the **new-connection pace**: 13 890 attempts in 540 s
is exactly the 25/s cap, and only 1 674 of them (12 %) reached a peer that accepted TCP —
DHT peer lists are mostly stale or NAT-ed. Attempts queue for a pace slot, which is why a
successful fetch takes 16 s on average and 1 187 hashes ran out of their 30 s budget. Pace
slots are reserved ahead of time (`Pace::reserve`): when a hash finishes or times out, its
still-waiting attempts are dropped and the slots they reserved go unused, so the real
connection rate is below 25/s (≈ 5 600 of the 13 890 started sessions ended without any
recorded outcome, most of them in this state — not measured precisely). This run predates
D66 (commit "router-measured TCP bounds"), which raised the default to 150 attempts/s with
≤ 350 open connections after a benchmark of the user's router (knee ~200 attempts/s,
docs/research/router-bench.md); the run at the new defaults follows.
Replacing slot reservation with a polled token bucket (a dropped attempt then costs nothing)
remains open for stage 8 tuning.

**Live run at the D66 defaults** (same setup, scratch database `dht_stage2_live3`, 450 s,
150 TCP attempts/s, ≤ 350 open, health probe on; a 202 s run before it on `dht_stage2_live2`
was stopped from outside the process and is only used as a cross-check: 100 verified/min,
25.8 %):

| | run 1 (25/s, above) | run 3 (D66 defaults) |
|---|---|---|
| unique infohashes / sampling paused | 11 175 / 497 s of 540 | 13 312 / 73 s of 450 |
| hashes collected / with peers | 4 138 / 3 176 | 3 774 / 2 799 (74 %) |
| metadata verified (per minute) | 522 (58) | **716 (95.5)**, 7 hybrid |
| success rate (verified / finished fetches) | 17.9 % | **25.9 %** |
| mean time of a successful fetch | 16.4 s | **2.1 s** |
| TCP attempts (per s) / established | 13 890 (25.7, at the cap) / 1 674 | 13 658 (30, cap 150) / 2 363 |
| hash-level `timeout` | 1 187 | 94 |
| KRPC queries/s (answered) | 134 (51 %) | 120 (50 %) |
| bandwidth in / out | 0.73 / 0.12 Mbit/s | 1.00 / 0.11 Mbit/s |
| CPU / peak RSS | 2.0 % of a core / 67.5 MB | 1.8 % / 64.2 MB |
| verified metadata | 25.1 MB | 40.3 MB (≈ 56 KB per info dict) |
| health probe | — | 90 probes, 6 failed, baseline 15.3 ms, last median 17.5 ms; brake engaged 3× |

**Reading (D66).** The connection pace no longer binds: attempts ran at 30/s against a cap of
150, successful fetches take 2 s instead of 16 s and hash-budget timeouts fell from 1 187 to 94.
The pipeline is still attempt-starved (3 774 of 13 312 queued hashes reached collection,
28 %), but the limit is now **peer collection**: ~17 KRPC queries per hash through iterative
lookups (the sampling node answered with peers for only 328 hashes), at 120 q/s. Raising the
share of hashes that get peers from the sampling node, or `MAGNETAR_DHT_QPS` towards the
router's measured 300 q/s knee, is stage 8 tuning. All three probe brakes fired 25–35 s
after the start, **before a baseline existed** (≥ 12 successful probes), on the "3 of 6
failed" rule; whether the start-up burst (bootstrap + 128 fetchers opening at once) really
saturated the router or a probe target dropped a few SYNs is **not established**. Its cost:
KRPC and TCP attempt rates halved for the first ~5.5 min (120 q/s average instead of ~160).
No brake fired after the baseline was set.

**Not verified in stage 2.** Storage of real DHT names (deliberately: no blocklist yet, D65);
the Tantivy index and app-side visibility of DHT rows (stage 5 readers); uTP-only peers
(TCP only); behaviour past 9 minutes (retry of `unscreened` rows after 6 h, the 1 h / 6 h /
24 h backoffs on real failures); memory with 16 MiB info dicts under load (the largest seen
live is not recorded; only the 48 KB mean); passive capture with an open port (announcer path
exercised only in the fake-network tests); IPv6 peers (dropped, BEP 32 later).

## 15. Stage 3: built and measured (2026-10-26)

**Code.** `crates/core/src/blocklist/` (matcher, `terms.v1` hashes; D68),
`crates/core/src/classify/` (classifier, `adult.txt`; D69), `scripts/blocklist-hash.sh`,
`crates/server/src/dht/classify.rs` (anitomy-ng anime refinement), the real screen in
`server::dht::screen` (`BlocklistScreen`, `install_from_config`), classification before the
screen and `reclassify_batch` in `server::dht::persist`, `Rejection::Blocked` and
`Store::{is_blocked, put_blocked}` on Nostr ingest, `SearchQuery.include_adult` through the
index (`SCHEMA_VERSION` 3), the SQL fallback, the API (`adult=1`), the phone
(`FfiConfig.show_adult`) and Android Settings (D70), and the IMDb/anime enrichers and attrs
backfill on `torrent_rows`. No new dependency (the stage 3 review made the workspace's
`unicode-normalization` a plain `core` dependency, D71).

**Order on the DHT path** (§7.1, as built): verify → parse → `classify` (pure, silent) →
`Screen::verdict(all_names, adult)` → block (hashes only) or store with `category` and
`classifier_version`, release attributes parsed with that category. At crawler start, rows of an
older classifier version are re-classified and re-screened (500 per transaction).

**Tests** (neutral placeholder terms only: `zorblax` hard, `wibblewob` context, `甲乙` / `丙丁`
for CJK). Unit: hashes equal to `sha256sum` of the script's input (ASCII and CJK), every
variant (diacritics, substitutions, separators, bigrams, padding paths), context tier only
for adult, hard-only texts, explicit vs ordinary age markers, strict loading that never echoes
a line, `Debug` without terms; classifier byte classes, games, anime, adult (weak pairs, the
film *xXx*, `.ass` subtitles, adult words with audio bytes), version 2 rules (JAV codes vs
years/serials/`WEB-DL`, CJK words, adult archives, `Game of Thrones … .rar`, doujinshi).
Postgres (`server/tests/it/dht_classify_pg.rs`): Nostr ingest blocks on title, file name
(substitution variant), description and hashtag; the context tier only for a `porn` category or
an adult name; a later harmless event for a blocked hash is rejected; 6 hashes + 6 event ids
kept and no text in `events`; a Nostr block deletes a DHT row of the hash; stage 2 rows
re-classified (one now blocked, origin `sweep`); DHT rows in the seed-check schedule with no
trackers; a malformed or missing `MAGNETAR_BLOCKLIST_FILE` fails without echoing the line.
`dht_fetch_pg.rs::pipeline_blocklist_classifies_and_never_logs_names`: 12 torrents through fake
DHT → fake seeder → Postgres with the real screen, the whole process logging at `TRACE` into a
buffer: one block per tier (hard in a padding path, context, age), stored rows with `video`,
`porn`, `anime`, and none of the names or paths in the logs or the JSON report.
`search_index.rs::adult_hidden_by_default_on_both_paths` (index = SQL fallback; a Nostr `video`
event whose DHT row is `porn` is hidden too), `enrich_pg.rs::dht_only_torrents_are_enriched_and_parsed`
(a DHT-only name matched to IMDb by title, attributes re-parsed by the backfill), Android
`SettingsTest` (switch off by default, mapped to `FfiConfig`).

**Live runs** (release build, home IP, aggressive, 4 virtual nodes, no UPnP, no router port
opened; scratch database `dht_stage3_live`, 420 s each; run 1 with classifier v1, run 2 with
v2 after re-classifying run 1's rows at start):

| | run 1 (v1) | run 2 (v2) |
|---|---|---|
| unique infohashes | 10 789 | 10 851 |
| verified info dicts (per min) | 672 (96) | 815 (117) |
| stored / blocked (by tier) | 669 / 3 (hard 1, context 2) | 808 / 7 (hard 1, context 6) |
| re-classified at start (now blocked) | — | 669 (6) |
| fetch success rate | 26.5 % | 29.3 % |
| CPU / peak RSS | 7.8 s (1.8 % of a core) / 74 MB | 11.2 s (2.7 %) / 107 MB |
| bandwidth in | 0.98 Mbit/s | 1.49 Mbit/s |

Blocked torrents were counted by tier only; their names were never printed or viewed (D53).

**Category distribution** after both runs (1 470 stored rows, classifier v2): video 644
(43.8 %), **porn 261 (17.8 %)**, audio 147 (10.0 %), other 102 (6.9 %), book 96 (6.5 %),
application 75 (5.1 %), anime 75 (5.1 %), game 56 (3.8 %), picture 14 (1.0 %). Storage:
`dht_torrents` ≈ 5.2 KB per row with its indexes, `torrent_attrs` ≈ 0.44 KB; every row got
attributes and a seed-check schedule row.

**Accuracy, hand-checked** (name, file count, size and byte share of the top extensions; an
"x" for names nobody could place, excluded):

| | sample A (100 from run 1) | sample B (100 fresh from run 2) |
|---|---|---|
| classifier v1 | 61 / 91 (67 %), adult recall 7 / 24, no false adult | — |
| classifier v2 | 85 / 91 (93 %; **tuned on this sample**), adult 22 / 24, no false adult | **78 / 94 (83 %)**, adult recall 19 / 29 (66 %), precision 19 / 19 |

Sample A drove v2 (CJK adult words, JAV-style codes, game data archives, CJK book hints,
adult archives), so its v2 figure is optimistic; sample B was drawn afterwards and not tuned
on. Sample B's 16 errors: 10 adult torrents not tagged (performer or site names only, Chinese
voyeur vocabulary, Korean words, adult doujinshi/comic magazines filed as book or other), 2
anime without the fansub shape filed as video, a PS4 `.pkg` game as application, a Unity game
and a doujinshi zip as other, a Bach box set ISO as application. Adult doujinshi count as
adult in both samples (hiding them by default is the point). Two sample A rows were blocked
by the v2 re-screen (counted, not identified).

**Found in the live sample**: one stored picture set in sample B was labelled with a child's
age (an explicit `NNyo` annotation) and carried no adult word, so neither the hard terms nor
the adult-only age rule caught it. It was deleted from the scratch database by hand (hash kept
as `sweep`, name not recorded anywhere), and the age rule changed: explicit age annotations
next to an age below 18 now block any torrent (D68). The same pass added CJK character n-grams
to the matcher, because names in Chinese and Japanese are single tokens.

**Not verified in stage 3.** Blocklist recall (only the vectors and the live counts; the
binding rule forbids checking what it matched); the Nostr sweep of already stored data (stage
5) — production Nostr data has not been screened by this list; the phone keeps events it
cached before the update (it only rejects new ones); classification of names in scripts and
languages beyond the samples; DHT rows in search results (stage 5 readers); the app switch on a
phone (built and unit-tested only); TMDB decoration of DHT rows (follows the IMDb/anime links,
not tested with DHT rows); the duration of the re-classification on a large table (669 rows
here, not timed).

**Stage 3 review fixes (D71).** (1) Seventeen placeholder bypasses of the first matcher, all
`Allow` before and `Block(Hard)` now: zero-width space/joiner, soft hyphen, word joiner, BOM,
`NUL` (the stored title drops it, so `zor\0blax` was screened as two words but stored as one),
a decomposed or stacked combining accent, fullwidth, mathematical bold and circled letters,
Cyrillic/Greek look-alikes, dotted and spaced-out letters (also inside a phrase and with
substitutions); fullwidth digits now feed the age rule. All 79 built-in hashes still
reproduce through the new normaliser (ignored test `builtin_hashes_reproduce_through_the_normaliser`
with `BLOCKLIST_PLAIN=<plain list>`, prints counts only). (2) Memory on hostile input: the
matcher kept every n-gram string of a check in a set and the classifier did the same for the
adult words (≈ 1 GB for a 16 MiB CJK info dict, ×`decode_parallel`); the matcher now streams
with a bounded keyed hash cache and the classifier reads ≤ 4 KiB per text. Measured (release,
this laptop): 16 MiB repetitive CJK + 100 000 paths 3.5 s, 16 MiB random CJK 8.6 s. (3) The DHT
writer classified and screened inside its open transaction on an async worker; it now does the
whole batch first, under `block_in_place`. (4) Re-classification paged with `LIMIT` over
`classifier_version < N` from the start of the key each time (quadratic on a large table, no
index on that column); it now pages by `infohash > last`. (5) Blocked rows left their
`torrent_attrs` row (parsed vocabulary only, no names) behind; DHT blocks, re-classification
blocks and Nostr blocks now delete it unless a Nostr event owns the infohash.

**Still open after the review.** The phone hides an infohash when *any* cached event is filed
`porn`, the server only when the canonical category is `porn` (or a DHT row is): the two differ
for an infohash with a non-canonical `porn` event. A Nostr event stored *before* its infohash
was blocked (by the DHT or another event) stays until the stage 5 sweep. Re-classification
re-screens only the first 5000 stored files of a row (`FILES_MAX`), not every name of the
original info dict. A hostile 16 MiB info dict costs up to ~9 s of one core to screen.


## 16. Stage 4: product integration and ops, built and measured (2026-10-26)

**Code.** Index: `search_index/{load,schema,mod}.rs` (`SCHEMA_VERSION` 4, DHT documents,
`origin` stored/facet/filter, `eligible_trust`, DHT file matches); SQL fallback
`search.rs` on `torrent_rows` (+ `search::explain_hits`); `store.rs` DHT-only and mixed
detail, origin refinement of fallback pages; `stats.rs` (`origin` breakdown, DHT metrics,
`recompute_published` Nostr-only); `api.rs` (`origin=` parameter, features `origin` / `dht`,
`StatsResponse.dht`, `StatsHealth.dht`); `dht/status.rs` (`DhtService`, `DhtStats`, the
per-minute sampler), `dht/upnp.rs` (D72); core types `Origin`, `DhtInfo`, `DhtStats`,
`UpnpStatus`, `SearchQuery.origins`; ffi `FfiOrigin`, `FfiDhtInfo`, `FfiDhtStats`,
`Magnetar::server_dht`; app (badge, Source chips, detail card, Stats, Status, `DhtFormat.kt`);
`docker/compose.dht.yml`, `compose.yml` pass-through, `.env.example`. New dependency:
`igd-next` 0.18.0 (+ `xml-rs`, `xmltree`, all MIT), server feature `upnp`.

**Trust treatment as built (D51/D73)**: a document with a DHT row passes every `min_trust`
(index: `eligible_trust` lifted to 1000; fallback: `t.origin = 'dht' OR …`). A Nostr event
stays canonical when one exists; a DHT-only result has empty `event_id`/`pubkey`/`npub`, trust
0 and `publisher_count` 0, and the app shows a neutral "DHT" badge instead of the trust star.
A blocked publisher hides its events, not the DHT row. Adult DHT rows stay hidden by default
(D52); the Source chip (`origins`) is an optional filter, never a default.

**Tests.** `server/tests/it/dht_search_pg.rs`: both search paths agree at `min_trust` 0; at 20 and
100 DHT rows still pass (and only they at 100); blocked publisher + DHT row → DHT-only result
and detail; Source chips on both paths (`dht`, `nostr`, both, bogus); disjunctive `origin`
facet; DHT titles and file names searchable, `matched_file` from the DHT files; the index
follows DHT inserts/deletes through `search_changes`; detail (DHT-only, both, Nostr-only);
published-per-day counts without the DHT-only hash; the fallback plan reaches `dht_torrents`
through `dht_torrents_search_tsv` with seq scans disabled (the trigram arm folds away).
`search_index.rs::adult_hidden_by_default_on_both_paths` now records the accepted fallback
difference (category filter on any row, DHT rows included). `server/tests/it/dht_upnp.rs`: an
in-process fake IGD (SSDP over UDP, device/service XML and SOAP over HTTP on 127.0.0.1, no
multicast): UDP and TCP mapped through unicast discovery with our LAN address and a 1 h lease;
725 → lease 0; 718 → `partial` with the error; the loop maps, then removes everything on
stop; after a renewal round in which every mapping fails, the lease-0 mappings of earlier rounds
are still removed at stop (within one 2 s budget); a port lost to another host (718 at renewal)
is not removed; no gateway → `failed`, softly; the crawler (fake DHT, 2 virtual nodes) maps exactly its
bound UDP ports, shows in `/health` (`dht`, `origin`) and `/api/v1/stats.dht`, and unmaps at
stop. Android `DhtTest` (chip ↔ query, breakdown fields per feature, labels, crawler lines,
uptime) with the existing unit tests, `assembleDebug`.

**Live run** (D75; release build, home IP, aggressive, 4 virtual nodes, `MAGNETAR_DHT_UPNP=false`,
no router port opened). Database `dht_stage4_live` = `pg_dump` of production (`magnetar-db-1`,
read-only, 1.8 GB custom format, 3 min) restored on the test Postgres (12 min): 501 820
infohashes, 506 988 events, 8.39 GB. Phase A without crawler: the DHT migration took **61 ms**,
the `SCHEMA_VERSION` 4 rebuild **48 s** for 501 820 documents (400.6 MB). Phase B: `serve` with
the crawler for 10.7 min (other daemons as in production except downloads, NIP-66, IMDb and
anime datasets off; Nostr crawl limited to 2 relays):

| | |
|---|---|
| unique hashes / queued / given to peer collection | 11 191 / 10 978 / 4 978 |
| info dicts verified | 851 (~80/min once running), 25.7 % of finished fetches |
| stored / adult / blocked | **837** / 154 (18.4 %) / 14 (counted by number only) |
| categories | video 394, porn 154, audio 98, other 60, book 43, application 36, anime 23, game 19, picture 10 |
| KRPC | 101–172 q/s, 45–49 % answered; **probe brake on from minute 1 to the end** (D66) |
| incoming queries without an open port | ~41/min |
| whole server (index, seed checks, Nostr crawl included) | 73.6 s CPU in 558 s = 13 % of one core; peak RSS 462 MB (index mmap included) |
| growth | database +8.2 MB: `dht_torrents` 5.05 MB = **6.0 KB per stored torrent** with its indexes, `dht_seen` 1.79 MB for 10 130 rows (~177 B), attrs/schedule/liveness the rest; index +2.6 MB |
| `stats_series` after the run | `dht_torrents_added` 837, `dht_hashes_seen` 11 179, `dht_fetch_ok` 807, `dht_fetch_fail` 2 324, `dht_blocked` 13 |

Search latency over HTTP (`POST /api/v1/search`, loopback, 48 queries × 5 runs after one warm-up:
the D43 benchmark set, plus `origins` dht / nostr on five texts, adult shown, `min_trust` 100;
client-side wall time, so HTTP and JSON included):

| when | p50 | p95 | p99 | max |
|---|---|---|---|---|
| before the crawl (0 DHT documents) | 8.9 ms | 16.3 ms | 22.8 ms | 24.5 ms |
| crawl, minute 5 | 14.0 | 30.4 | 56.6 | 65.2 |
| crawl, end of minute 10 | 40.4 | **81.6** | 105.1 | 129.8 |
| after the crawl (837 DHT documents, 683 shown, crawler off) | 11.5 | 24.4 | 34.4 | 36.6 |
| second crawl (5 min, 1 149 stored at the end), each minute | 9.1–9.3 | 17.5–22.6 | 25.9–29.7 | ≤ 43.2 |

p95 stayed under the 100 ms bound in every measurement. The end-of-run sample is the outlier
(two queries above 100 ms p95: `game of thrones s01`, whose file match reads paths from Postgres,
and `french` by date); the second crawl did not reproduce it, and its cause (Postgres load from
seed-check batches and DHT writes at that moment, other load on the laptop) is **not
established** (load average and Postgres activity were only recorded in the second crawl: load
~2, 2 active backends). Facet `origin` on the copy afterwards: nostr 465 030, dht 683 (adult
hidden); `origin=dht&min_trust=50` returned the 683 DHT documents with trust 0 and no publisher.

### 16.1 Ops

- **Turn it on**: `MAGNETAR_DHT=aggressive` (or `polite`) and restart `serve`; in Docker
  `docker compose -f compose.yml -f docker/compose.dht.yml up -d` (docs/DOCKER.md). The first
  start after the upgrade runs the DHT migration (61 ms on the production copy) and rebuilds the
  search index once (`SCHEMA_VERSION` 4, 48 s).
- **Ports**: UDP `MAGNETAR_DHT_PORT`…`+NODES-1` (6891–6894). `serve` maps them with UPnP in
  aggressive mode (`MAGNETAR_DHT_UPNP=false` to forward by hand or not at all); in Docker set
  `MAGNETAR_DHT_UPNP_GATEWAY` and `MAGNETAR_DHT_UPNP_LOCAL_IP`. No TCP port: the crawler never
  listens on TCP (D72).
- **Watch**: `/health` lists `dht`; `/api/v1/stats` and `/api/v1/stats/health` carry `dht`
  (`running`, `last_error`, `hashes_per_min`, `queue_len`, `metadata_per_min`,
  `fetch_success_rate`, `stored_total`, `adult_total`, `blocked_total`, `brake`,
  `incoming_per_min`, `upnp.state` / `mappings` / `last_error`); the app shows them on the Stats
  health card and the Status screen; the activity chart has "DHT torrents added" and "DHT hashes
  found". `incoming_per_min` near zero for 10 min with `upnp.state = mapped` means the router or
  a firewall drops unsolicited UDP. `brake = true` for long stretches means the D66 probe sees a
  slow or lossy line (it did for the whole stage 4 run).
- **Stop**: SIGTERM/Ctrl-C; within `serve`'s grace the crawler saves its identity and 1000
  contacts and removes its UPnP mappings.
- **Disk**: no cap (user decision). Measured ~6 KB per stored torrent in `dht_torrents`, ~10 KB
  per stored torrent for the whole database including the `dht_seen` queue of hashes not
  (yet) fetched, plus ~3 KB of index. At the stage 4 rate (~80 stored/min ≈ 115 k/day) that is
  roughly 1 GB a day; `db_size` in Stats shows it.
- **Bounded measurement**: `magnetar-server dht --seconds N` never maps router ports unless
  `MAGNETAR_DHT_UPNP=true` is set explicitly.

**Not verified in stage 4.** UPnP against the real router (only the fake gateway; the router was
not touched) and from a container; inbound reachability and passive capture with mapped ports;
the app on a phone (built and unit-tested only); the SQL fallback's speed at production size
with DHT rows (only the plan shape, on a test database); `both`-origin torrents at scale (the
crawler skips hashes known from Nostr, `MAGNETAR_DHT_FETCH_KNOWN=false`, so none appeared in the
run: they arise only when a Nostr event arrives later for a DHT hash); the cost of the
`DhtStats` totals query (`count(*)` on `dht_torrents` and `dht_seen`, cached 60 s, one refresh
at a time, 5 s statement timeout after which the last value is kept) at millions of rows; the cause of the end-of-run latency outlier; the Nostr sweep of §7.1
(not built).

## 17. Final integration check (2026-10-26, no new features)

- **Merge**: `main` (`cb5dd61`) is an ancestor of `dht/crawler`; `git merge-tree` is clean (the
  merge is a fast-forward).
- **Migrations from scratch** (empty database, release build): all 11 applied, 624 ms of
  migration time in total (38–81 ms each, `20261101000000_dht` 70 ms); a second `migrate` is a
  no-op.
- **Production copy**: `pg_dump -Fc` from `magnetar-db-1` (read-only, 230 s, 1.80 GB), restored
  with `pg_restore -j 4` into `dht_final_prod` on the test Postgres (~9 min): 501 854
  infohashes, 507 022 events, 8.40 GB, migrated to `20261031000000`. The DHT migration took
  **47.8 ms** (114 ms for the whole `migrate` process).
- **Gates**: `cargo fmt --check`, `clippy --workspace --all-targets --all-features -D warnings`,
  `cargo test --workspace --all-features` (480 passed, 0 failed, 23 ignored network tests);
  `scripts/build-android.sh` with `assembleDebug testDebugUnitTest --rerun`: debug APK built,
  37 app unit tests passed. Docker: `docker build` 354 s, image 167 MB, `--version` →
  `magnetar-server 0.2.0`, runs as uid 10001; image removed afterwards.
- **Live run** (release build, home IP, aggressive, 4 virtual nodes on UDP 16891–16894,
  `MAGNETAR_DHT_UPNP=false`, no router port opened; daemons as in §16 (downloads, NIP-66, IMDb,
  anime datasets off; Nostr crawl 2 relays); fresh index directory, so the `SCHEMA_VERSION` 4
  index was built from nothing while crawling). 614 s from start to exit:

| | |
|---|---|
| index build (502 k documents, crawler and seed checks running) | **132 s**, 405 MB |
| unique hashes / queued / given to peer collection | 11 855 / 11 653 / 5 114 |
| metadata verified / stored | 1 152 / **1 146** (~113/min once running), 31.3 % of finished fetches |
| adult / blocked | 193 (16.8 %) / 6 (counted by number only) |
| categories | video 537, porn 193, audio 131, book 96, other 64, application 55, game 39, anime 24, picture 7 |
| KRPC | 196 q/s in the first 15 s, then 151 → 99 q/s with the **probe brake on from second 55 to the end** (D66); 58–62 % answered; 43–73 incoming queries/min without an open port |
| whole server | 224.9 s CPU in 613 s = 37 % of a core (index build included); peak RSS 852 MB |
| database growth | +10.98 MB = 9.6 KB per stored torrent; `dht_torrents` 6.50 MB (5.7 KB each), `dht_seen` 1.88 MB for 10 504 rows |
| stop | SIGINT → exit in < 1 s; `dht.identity` and `dht.nodes` (1000 contacts) saved; UDP and TCP ports released |
| log | 0 errors; 161 warnings: 43 probe-brake notices, 118 sqlx slow-statement (104) / slow-acquire (14) notices, 114 of them during the index build and 4 at shutdown |

Search latency (`POST /api/v1/search`, loopback, the §16 set of 48 queries × 5 runs):

| when | p50 | p95 | p99 | max |
|---|---|---|---|---|
| index just ready (crawl, minute 2.5) | 11.3 ms | 20.6 ms | 26.4 ms | 28.5 ms |
| crawl, minute 5 | 9.6 | 23.0 | 31.5 | 39.2 |
| crawl, minute 9 | 10.8 | 23.1 | 32.7 | 35.1 |
| restart with `MAGNETAR_DHT=off` (index reused, ready in 840 ms; 953 DHT documents shown) | 9.4 | 18.3 | 22.2 | 24.0 |

The §16 end-of-run outlier (p95 81.6 ms) was not reproduced. Slowest query in every sample:
`game of thrones s01` (file match, 24–39 ms p95).

**Smoke test** (all passed, during the crawl): `/health` features `origin` and `dht` (and
without the crawler: `origin` only, `stats.dht` null); `GET /api/v1/search?origin=dht&min_trust=100`
→ DHT-only hits with empty publisher and no adult category; `adult=1` adds the adult ones
(270 → 317 at that moment); `origin=bogus` → 400; `POST` search with `origins` and
`include_adult`; `GET` / `POST /api/v1/facets` with `origin`; `/api/v1/stats/breakdown?field=origin`;
torrent detail of a DHT-only hash (`summary.origin = dht`, `dht` block, trust factor `dht`, empty
pubkey); swarm graph of a DHT hash; `/api/v1/stats.dht`; `/api/v1/stats/health.dht`;
`/api/v1/stats/timeseries?metric=dht_torrents_added,dht_hashes_seen`.

**Not verified here**: UPnP against the real router or from a container, inbound reachability
with mapped ports, the app on a phone, the Docker image running the crawler (only `--version`),
the Nostr ingest `blocked` rejection over HTTP (covered by the Postgres tests only: building a
live blocked event would mean looking at the list), and a latency baseline on this copy before
any DHT row (the index was built during the crawl; §16 measured 16.3 ms p95 before).

## 18. Throughput work on a public line (D93, 2026-10-27)

Starting point (production, direct fiber line, public IP, `QPS 3000`, `PEER_LOOKUPS 2048`,
`FETCH_CONCURRENCY 2048`, `TCP_RATE 1000`, `TCP_MAX 4000`): ~2 700 metadata/min, KRPC at its
cap, answered share falling 51 % → 43 % in 10 min, ~1 core. A packet-level study
(`~/.cache/peer-reuse-research/REPORT.md`) found lookups running their whole 10 s budget, answers
p50 100 ms / p99 559 ms / 0.25 % after 1 s, a ~1.2 s single-row `mark_failed` in the collector,
announces lost after the announcer's own `get_peers`, and "accept-all" fake peers.

### 18.1 What changed

- **Lookup soft timeout and branch widening** (`MAGNETAR_DHT_LOOKUP_TIMEOUT_MS`, default 4000 since the production A/B in §18.5,
  `core::dht::lookup`): a lookup waits for at most 4 queries; one unanswered 1.5 s after it was
  **sent** turns *slow*: its slot goes to the next candidate and it no longer holds up the end of
  the walk (it is left out of the closest-8 test), but its answer is still used until the 4 s
  hard timeout (libtorrent's short timeout). A lookup has at most 8 queries out, slow ones
  included. The `get_peers` to the sampling node uses the same timeout: past it the lookup
  starts, and a late answer with peers still wins (the walk is dropped). Sampling, pings and
  bucket refreshes keep the 4 s timeout.
  *First build mistake, found live*: the soft timer started when a query joined the global pace
  queue, and the transaction was registered before the pace wait. With 1024 lookups the pace
  queue alone is seconds long, so nearly every query went "slow" before it left, lookups queued
  ever more, and the pending map (cap QPS × 4 s × 2) refused 350 k queries as `busy`
  (445 metadata/min instead of ~1 100). Now the timer runs from the send, and a transaction
  exists only once its datagram is on the wire (`Dht::query_then`); tests
  `the_soft_timeout_starts_at_the_send`, `the_pace_queue_does_not_fill_the_transaction_map`.
- **Early stop counts usable peers only** (bug): "enough peers" counted every `values` entry,
  also those of node-farm answers (ignored in the result) and non-global addresses (dropped
  before the fetcher), so one such answer could end a walk with nothing usable.
- **Status writes out of the collectors**: `no_peers` failures go to a writer task (≤ 500 rows
  or 1 s, one `UPDATE … FROM unnest(…)`); a hash listed `n` times ends as after `n` single
  writes (test `batched_failures_match_single_writes`). The old build at 1024 collectors
  exhausted the Postgres pool with single-row updates: 415 "pool timed out" warnings in 10 min,
  and triage batches failed with it.
- **Triage off the receive loop** (bug): the loop awaited each Postgres triage statement
  (mean 126 ms, up to 28 s measured), and meanwhile the engine's 10 000-sighting channel
  overflowed: 14 896 (old build) to 152 876 sightings dropped per 10 min, never even recorded.
  Batches now go to a triage task; when it is 16 batches behind, the current batch keeps
  growing (up to 100 000 sightings, then new ones are dropped and counted as
  `triage_dropped`), so the loop never waits on Postgres.
- **Announces after the seen filter** (bug, 0 rows `via = announce` in production): an announce
  with a peer for an already-seen hash moves the queued hash to the announce lane with that
  peer, hands the peer to a collection in flight (it goes first in the fetch), or goes to
  Postgres triage again (where the existing 10-min retry of a `failed` row now actually
  applies; before, the seen filter hid every announce from it for 6–12 h). A sighting of higher
  priority also moves a queued hash up (`FetchQueue::push`). `dht_seen.via` is set to announce.
- **Fake-peer reputation** in `core::metainfo` (no middleware, no database, no address ranges): a
  peer (`ip:port`) that accepts our BitTorrent handshake **for the hash** and then never sends
  its BEP 10 extended handshake gets a strike (two when its peer id starts with `-XT` or
  `cf-aa84-`, which only doubles a strike the behaviour earned); 3 strikes → skipped for 1 h;
  strikes are forgotten 1 h after the last one, verified metadata clears them; LRU of 50 000
  entries. Skipped peers do not use up the per-hash cap; a hash whose only peers are skipped
  fails as `fake_peers`.
- **Peers per hash** configurable (`MAGNETAR_DHT_PEERS_PER_HASH`, default 24 since §18.5, 1–64): the fetcher's
  cap and where a lookup stops.
- **Stats** (`DhtStats`, serde default, §10): `collected`, `from_source`, `looked_up`,
  `announce_fetches`, `announce_upgraded`, incoming queries per type, `fake_peers_skipped` /
  `_banned` / `_tracked`, `lookup_mean_ms`, `queries_per_lookup`, `lookup_slow_share` (over the
  rate window), and **answer share by node age**: queries to pool nodes < 5 min old, older ones,
  nodes not in the pool, and pool nodes asked again after answering a sample (`repeat`). The
  `dht` command's report has the same plus triage timings, and logs one line a minute with
  these rates.

### 18.2 Measured (laptop on the same fiber line, production crawler running alongside)

Release builds, `magnetar-server dht --mode aggressive --seconds 600`, fresh scratch database
each (bootstrap from the routers), UDP 17891–17894 (before) / 18891–18894 (after),
`QPS 1000`, `PEER_LOOKUPS 1024`, `FETCH_CONCURRENCY 2048`, `TCP_RATE 1000`, `TCP_MAX 4000`,
`PROBE off`, no UPnP. Runs are sequential, so DHT conditions differ between them by a few
percent.

| run | metadata/min | fetch success | collected/min | q/s sent | answered | queries/lookup | lookup mean | TCP attempts/success | open TCP mean / max | CPU (of a core) |
|---|---|---|---|---|---|---|---|---|---|---|
| old build (`ab51607`) | 1170 | 29.8 % | 6540 | 844 | 56.8 % | n/a | n/a | 16.8 | 702 / 1157 | 0.30 |
| first D93 build (timer before the pace wait) | 446 | 25.5 % | 5149 | 377 | 67.6 % | 25.8 | 5.9 s | 18.2 | 286 / 690 | 0.15 |
| timer fixed, triage still inline | 1025 | 29.9 % | 5679 | 803 | 56.8 % | 16.8 | 7.8 s | 17.2 | 621 / 1119 | 0.21 |
| D93 (triage in its own task), 12 peers/hash | 1031 | 29.8 % | 5561 | 794 | 58.8 % | 16.9 | 8.0 s | 17.4 | 594 / 1218 | 0.33 |
| D93 (triage in its own task), lookup timeout 4000 (no widening) | 1177 | 30.1 % | 6128 | 859 | 57.2 % | 15.2 | 7.9 s | 16.9 | 635 / 1154 | 0.28 |
| D93 (triage in its own task), 256 lookups | 568 | 30.4 % | 2492 | 979 | 46.7 % | 32.1 | 6.2 s | 18.4 | 344 / 472 | 0.20 |
| D93 (triage in its own task), 24 peers/hash | 1153 | 34.8 % | 5399 | 806 | 54.7 % | 17.5 | 8.2 s | 18.2 | 694 / 1307 | 0.23 |
| D93 (triage in its own task), 48 peers/hash | 1131 | 34.7 % | 5340 | 789 | 58.2 % | 17.9 | 8.5 s | 21.7 | 848 / 1525 | 0.21 |

The last change (the triage hand-off that never waits, below) came after these runs and was not
measured live; in the D93 runs triage took up to 5.3 s, 15.6 s and 27.9 s (busy disk shared with
the production database), and in the 4000 ms run 152 876 sightings were still dropped while the
loop waited for its 16-batch slack.

Old build: `marked_no_peers` 25 506, 415 pool-timeout warnings, 14 896 sightings dropped,
484 316 lookup queries refused by our own per-IP limit (1 q/s, D54) against 506 623 sent.

**Reading it.** At this setting the soft timeout does not raise throughput: the same build with
the timeout at 4000 ms (no widening) did as well or better (1 177 vs 1 031 metadata/min, 15.2 vs
16.9 queries per lookup, same ~8 s lookups). Lookups are bounded by the KRPC budget, not by
silent nodes. Only ~15 % of lookup queries go slow once the timer runs
from the send; the mean lookup stays ~8 s because each query first waits in the global pace
queue (1024 lookups × 4 queries at ~800 q/s ≈ 5 s per round), and sent queries per collected
hash stay ~8 (before 7.7). Fewer lookups (256) reach the KRPC cap (980 q/s) but collect less
(2 450/min, 32 queries per lookup, sampling starved, 5 283 hashes left queued): the per-IP limit
refuses fewer of their queries, so each walk sends more. What did help: **24 peers per hash**
(success 29.8 % → 34.8 %); 48 gives nothing more (34.7 %, 1 131/min) for more TCP (21.7 attempts per success, up to 1 525 connections open). The fixes for the pool exhaustion and the dropped sightings
remove losses that grow with load (production runs at 3× this rate).

**Answer share by node age** (the 51 % → 43 % question): in every run the overall share falls
over 10 min (e.g. 0.73 → 0.48), while pool nodes asked again after answering a sample keep
answering **94–97 %** throughout, and nodes in the pool for more than 5 min 0.64–0.84. The fall
comes from **young pool nodes** (0.73 → 0.35): contacts learned from answers, most of which never
answer. So it is not the same nodes rate-limiting us; it is the pool filling with dead contacts
and lookups seeding from them. Not changed here (candidates: prefer nodes that answered when
seeding lookups, or only pool nodes that answered once).

**Not verified**: the effect at production settings (QPS 3000, 2048 lookups) and over hours;
whether the fake-peer skips reach the ~10 % of accepted handshakes the study saw (skips in
10 min: 223, 381, 74, 305, 300 attempts in the five final runs, against ~180 k TCP attempts; 334–475 peers reached 3 strikes per run).

### 18.5 Production A/B and new defaults (2026-10-27)

Production server on the fiber line (QPS 3000, 2048 lookups, FETCH_CONCURRENCY 2048, TCP_RATE
1000, TCP_MAX 4000), 10 minutes per run, all runs within the same two hours on the production
database. Metadata stored per minute at the end of each run and the fetch success rate:

| build | lookup timeout | peers per hash | metadata/min | fetch success |
|---|---|---|---|---|
| old (`ab51607`) | 4 s (fixed) | 12 (fixed) | ~1 690 | 23 % |
| new | 1.5 s | 24 | ~1 340 | 25 % |
| new | 4 s | 24 | 1 800 → 2 160 | 26 % |
| new | 4 s | 12 | ~1 750 | 23 % |

At this load the 1.5 s timeout loses a fifth to a half of the output, and 24 peers beat 12, so
the defaults became `MAGNETAR_DHT_LOOKUP_TIMEOUT_MS=4000` and `MAGNETAR_DHT_PEERS_PER_HASH=24`.
The same old build stored ~2 700/min the night before: the DHT varies a lot by hour, so only
compare runs made close together.

__SECTION19__
