# Seed checks (liveness) at scale

How the server keeps seeders/leechers current for every torrent (ARCHITECTURE.md D45; the
protocols are D16, the phone's one-shot check still uses the fixed TTLs of D17). Research
notes: docs/research/bittorrent.md.

## What a check does

One batch (`MAGNETAR_LIVENESS_BATCH`, default 2000 infohashes):

1. **Pick** the due infohashes from `liveness_schedule`, lane by lane: viewed (detail page) >
   matches a saved search > shown in search results > the rest; in each lane the earliest
   due first. A never-checked infohash is due at `-born` (born = first publication), so the
   backlog drains newest first and ahead of every re-check. The query is one range scan of
   the `(prio, due_at)` index per lane and stops at the batch size.
2. **Trackers.** Hashes are grouped by tracker (`udp://host:port`, or the full http(s)
   announce URL). Each tracker gets full packets of 74 hashes (BEP 15; 50 per HTTP GET,
   BEP 48), at most `MAGNETAR_LIVENESS_TRACKER_RPS` requests per second (default 1), and up to
   `MAGNETAR_LIVENESS_TRACKER_CONCURRENCY` trackers are scraped at once. The scraper lives as
   long as the server: per tracker one UDP socket, its address (looked up again after 1 h)
   and its connection id (reused for 60 s, as BEP 15 allows), so a busy tracker sees one
   connect a minute. A tracker that fails (timeout, error action, HTTP error, bad answer) is
   skipped for 2 min, doubling per failure in a row up to 6 h; while it backs off its hashes
   count as unanswered for it.
3. **Torrents without a tracker** (two thirds of production) are scraped on the trackers
   their magnet links get (`magnet::DEFAULT_TRACKERS`: opentrackr, stealth.si,
   torrent.eu.org). Those answers only count when they report seeders: zero from a tracker
   the torrent never named means "not registered there", not "dead".
4. **DHT** (`MAGNETAR_LIVENESS_DHT`, BEP 33) only for hashes no tracker answered usefully,
   `MAGNETAR_LIVENESS_DHT_CONCURRENCY` walks at once (default 32), each at most 10 s. A hash
   goes to the DHT as soon as its last tracker is done, so walks overlap the scrapes. Walks
   start from a cache of 4096 nodes that answered earlier walks (the closest to the target
   first); the bootstrap routers are only asked while the cache is small. A walk ends when
   the 8 closest nodes answered (BEP 5's K).
5. **Store**, in one transaction with rows in infohash order: the result, the next due time,
   streaks, and a history point when one is due. A check that got no answer at all keeps the
   previous result (so an outage of a big tracker does not wipe seeders from search results)
   and only moves the schedule.

Batches run back to back while a full batch is due; otherwise the loop sleeps
`MAGNETAR_LIVENESS_INTERVAL_SECS` (default 60). The `liveness.enabled` setting still turns
the checker off.

### Node farms on the DHT

The live DHT answers many lookups from nodes that do not hold the swarm: one IP answering
from many ports with ids a few bits from every target (seen: three Hetzner addresses), and
ids that copy 15 bytes of the target (port 10240). Both answer every lookup with a made-up
peer, which made torrents nobody seeds look alive. Nodes on an IP seen with 3 or more ports
in one walk, and ids sharing 64 or more leading bits with the target (honest close nodes
share about 23), are not walked through and their values and Bloom filters are ignored
(`DhtScrape.ignored_answers`). On comparable random samples of the production copy the
share of "alive" results went from 69 % to 53 % with this filter; walks also stopped ending
early on such nodes. BEP 42 (node id bound to the IP) was considered and rejected: Transmission
nodes (port 51413) do not follow it and hold real swarm data.

## Schedule

`core::liveness::next_check`:

| Result | Next check |
|---|---|
| Answered, torrent ≤ 7 days old | 6 h |
| Answered, ≤ 90 days | 2 days |
| Answered, older | 4 days |
| Dead (answered, nobody seeding) 3 times in a row | 7 days |
| No answer at all | 1 h, doubling per check in a row, at most 1 day |
| Hot (viewed, saved-search match, in search results; for 3 days) | at most 6 h |

Every interval is spread by ±10 % so a backlog checked in one go does not come due in one
go. The stored result's `expires_at` is its next check: it counts as fresh (for "hide dead"
and the dead badge) until it is replaced.

**Boosts.** A search boosts what its page shows, a detail view the torrent (lane 3), and once
an hour the first 100 results of every saved search (lane 2). The API only records boosts in
memory; the loop writes them before its next batch. A boost also makes the torrent due at
once when its last check is older than 1 h (viewed), 6 h (saved search) or 12 h (search).
A forced check (`POST /api/v1/torrents/{ih}/liveness`, the app's "Check" button) is one-shot,
outside the rate limits, and goes through the same schedule.

## History

`liveness_history (infohash, at, seeders, leechers, peers_est)`: a point when a check
answered and the previous point is at least 5 h old (one per ~6 h at most), skipped while the
values only wobble (±1 and ±10 %, dead ↔ alive always counts) unless the previous point is a
week old. Kept 90 days, pruned hourly (BRIN index on `at`). The migration turns the existing
results into the first points.

`GET /api/v1/torrents/{infohash}/liveness-history?days=N` (default and max 90) →
`LivenessHistory {infohash, points: [{at, seeders, leechers, peers_est}], keep_days}`, oldest
first; draw it as a step line. 404 for an unknown infohash (and from servers older than D45;
`Health.features` lists `liveness_history`). The app reaches it through
`Magnetar::liveness_history` (None offline).

Size: 171 bytes per point with its primary key (measured). A worst case of one point per
check for every torrent (≈ 22 per 90 days at the 4-day interval) is ~11 M points, ~1.8 GB for
0.5 M torrents; with dead and steady torrents writing weekly points the expected size is well
under 1 GB. The schedule itself is ~100 MB for 0.5 M infohashes (heap + both indexes).

## Backlog stats

`GET /api/v1/stats` → `liveness` (null from older servers): `enabled`, `torrents`,
`never_checked`, `due_now`, `checked_24h`, `checks_per_hour` (infohashes checked in the last
hour), `alive` / `dead` / `unknown` (latest result: seeded / answered with nobody seeding /
no answer), `trackers` known to the scraper, `trackers_backed_off`, `dht_nodes` cached,
`last_batch` (a `BatchReport`: hashes, trackers, requests, failures, DHT walks, alive, dead,
unknown, ms) and `last_batch_at`. The counts are cached for 30 s (two scans of narrow
tables). The app's Status screen shows them, with the time the backlog needs at the current
pace, when the server sends them.

`magnetar-server liveness --limit N` runs batches now (configured rates) and prints one JSON
line per batch: the `BatchReport`, the time including the write, trackers and DHT nodes.

## Measurements (2026-10-26)

Production copy in a test database (`scripts/bench-search-db.sh 1`: 497 773 infohashes,
502 407 events, 49 579 results, read-only `COPY … TO STDOUT` from production), never-checked
order shuffled to get a random sample, real trackers and the live DHT, home connection,
default settings.

**Due query** (EXPLAIN ANALYZE): the old `WITH due AS (…)` (a parallel scan of `torrents`
joined to `liveness`, grouped and sorted) took 534 ms for 500 rows on the copy and 1.2–2.4 s in
production logs. The new one picks 2000 rows in 5.4 ms through `liveness_schedule_due`
(index scans that stop at the limit); with the trackers and the result row the whole
statement is 46–97 ms, and stays index-driven with 0.5 M result rows (nested loop on
`liveness_pkey`). The migration takes 12 s on the copy. The old `liveness_expires` index
served only the old query and is dropped (one index write less per check, and result
updates can be HOT).

**Throughput**, three batches of 2000 random torrents:

| | batch 1 | batch 2 | batch 3 |
|---|---|---|---|
| time | 245 s | 249 s | 244 s |
| tracker requests / trackers in batch | 134 / 47 | 80 / 47 | 135 / 50 |
| trackers failing / backing off at the end | 32 / 0 | 37 / 7 | 27 / 26 |
| answered by a tracker | 652 | 662 | 642 |
| DHT walks / with an answer | 1348 / 1284 | 1338 / 1264 | 1358 / 1279 |
| alive / dead / no answer | 1064 / 872 / 64 | 1039 / 887 / 74 | 1014 / 907 / 79 |

6000 torrents in 737 s: **29 k per hour**, CPU 23 s (user + system) = **3 % of one core**.
Extrapolated, the 0.5 M production backlog is cleared in **~17 h**. The busiest trackers (the
three fallback ones, which most torrents that name trackers list too) get nearly every hash
of a batch, 27 packets, one every ~9 s, far below the 1/s cap. DHT walks dominate (two thirds of the
hashes, ~5.5 walks per second, each 25–50 answering nodes plus timeouts, so a few hundred
small UDP packets per second spread over thousands of nodes; the bandwidth, a few tens of
KB/s each way, is an estimate, not a measurement). The first version (16 walks at once, walks ending only when
16 closest nodes answered, 2 s RPC timeout) needed 795 s per batch (9 k per hour, 54 h for
the backlog).

After the backlog the steady state is far lighter: with the intervals above (most torrents
are older than 90 days), 0.5 M torrents need about 5–6 k checks per hour.

To slow it down on a weak line: `MAGNETAR_LIVENESS_DHT_CONCURRENCY=8` (about a quarter of
the DHT traffic), `MAGNETAR_LIVENESS_TRACKER_RPS=0.5`.

## Search index

Result writes go through the `liveness` table, whose statement triggers append to the
search change log (D43), so seeders sorts and filters follow within about a second of a
batch's commit (`tests/liveness_pg.rs`). A check without an answer writes nothing there, so
it re-indexes nothing.
