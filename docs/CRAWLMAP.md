# Crawl map: where discoveries come from

Two views in the app's Stats tab (decisions: ARCHITECTURE.md D101–D105):

1. **Where discoveries come from** (Stats → Charts card, `GET /api/v1/stats/sources`): DHT
   hashes seen and torrents stored per discovery route, Nostr relays by how they were found
   and what they delivered, and each crawling machine (the server, every crawl worker).
2. **Crawl map** (screen opened from that card, `GET /api/v1/stats/map`): a world map coloured
   by count, switchable between DHT discoveries and Nostr relays, with "top countries" and
   "top networks (AS number + organisation)" lists.

## Privacy rule (D101)

**No IP address is ever stored, logged, sent to the app or written to disk** — not raw, not
hashed (an IPv4 hash can be reversed by trying all 2³² addresses). An address is turned into
(country, AS number, AS organisation) in memory at the moment it is counted, then dropped; only
aggregate counters by country / network are kept. In detail:

| Where | What is kept |
|---|---|
| `geo_counts_hourly`, `geo_counts_total` | `(layer, metric, [bucket,] cc, asn, value)`: counts by ISO country code and AS number |
| `geo_as_names` | AS number → organisation name (public registry data from the ASN database) |
| `dht_seen.src_cc`, `src_asn` | country / AS number of the node a hash was first heard from, to count "stored" later |
| `relays.geo_cc`, `geo_asn`, `geo_checked_at` | where a relay's host resolved to (the address itself is dropped) |
| worker → server triage request | per hash `cc` / `asn`, node counts by `cc` / `asn`, AS names; never an address |
| memory | the in-memory tally (codes and counts); "nodes heard from" dedupe = a Bloom filter over a SipHash keyed with a random key that is replaced, and the bits cleared, at every UTC day change (never written, never sent) |
| logs | counts and error classes only |

Relays are public servers, but the same rule applies: their resolved address becomes a country
and network and is dropped, and a relay whose URL *is* an IP literal is listed as
`wss://(IP address)` in the sources answer (the app also redacts such hosts on decode).

Tests (`crates/server/tests/it/crawl_map.rs`): the schema has no `inet`/`cidr`/`macaddr` column
and the crawl-map tables have exactly the code / count columns above; after a DHT and a relay
run every stored text value (`geo_*`, `dht_seen.src_cc`, `relays.geo_cc`, AS names) is free of
IP-looking strings and every country code is two capital letters or empty; every JSON answer
of both endpoints and the worker triage request is scanned for IP-looking strings (IPv4 and
IPv6 shapes) and address-named fields. The detector itself is tested on positives and
negatives.

**Not covered by this rule (older, operational state):** the DHT crawler saves its node
identity and up to 1000 routing contacts (addresses) so it can rejoin the network after a
restart (`settings` row on the server, `dht.json` on workers, D55/D100), and the seed checker
talks to trackers by address. These are needed for the crawler to work, predate D101 and are
not part of the crawl map; whether to drop or shorten them is the user's call.

## Data source: DB-IP Lite (CC BY 4.0)

- Databases: *IP to Country Lite* and *IP to ASN Lite*, MMDB format, updated monthly, licensed
  under [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/) by
  [DB-IP](https://db-ip.com). The licence asks for attribution and, where results are shown, a
  link back: every map answer carries `attribution` ("IP Geolocation by DB-IP
  (https://db-ip.com), CC BY 4.0"), and the app shows it as a link on the map screen.
- URLs (checked on db-ip.com/db/download/ip-to-country-lite and …/ip-to-asn-lite, 2026-10-27):
  `https://download.db-ip.com/free/dbip-country-lite-YYYY-MM.mmdb.gz` and
  `https://download.db-ip.com/free/dbip-asn-lite-YYYY-MM.mmdb.gz`. October 2026: 4.1 / 5.2 MB
  compressed, 8.3 / 9.6 MB unpacked (710 834 / 477 362 records); the SHA-1 shown on the page is
  of the unpacked file.
- Not in git. The server downloads both on first start into `<state dir>/geoip`
  (`dbip-country-lite.mmdb`, `dbip-asn-lite.mmdb`), checks every 24 h, and fetches the new
  month's release from the 2nd of a month (the previous month's when the new one is not out).
  A download is gunzipped with size caps (64 MiB compressed, 256 MiB unpacked), opened and its
  `database_type` checked *before* it atomically replaces the old file; a failed refresh keeps
  the old one.
- Reader: crate [`maxminddb`](https://crates.io/crates/maxminddb) 0.32.0 (ISC, maintained by
  oschwald, released 2026-09; deps `ipnetwork` MIT/Apache-2.0, `memchr`), without the `mmap`
  feature: the files are read into memory (country ~8 MB, ASN ~10 MB).

| Variable | Default | |
|---|---|---|
| `MAGNETAR_GEOIP` | `on` | `on` = country + ASN; `country` = country only (half the memory, no networks); `off` = nothing is downloaded or counted, the map answers `available: false, reason: "disabled"` |
| `MAGNETAR_GEOIP_DIR` | `<state dir>/geoip` (worker: `<worker state dir>/geoip`) | where the two files live; put them there by hand for offline use |
| `MAGNETAR_GEOIP_DOWNLOAD` | `true` | `false` = never download, only read the directory |

Without a database the map reports `available: false` with `reason` `missing` (not downloaded
yet / offline) or `invalid`, keeps serving the counts gathered earlier, and nothing else changes
(DHT, Nostr crawl, search are untouched).

## What is counted

### DHT (`layer=dht`)

The core DHT engine now says which node told us about each hash (`Sighting.from`): the node
whose BEP 51 sample held it, the node whose `get_peers` query reached us, or the announcer.

| Metric | Counted when | By |
|---|---|---|
| `hashes` | a sighting passes the seen filter (distinct hashes, as `dht_hashes_seen`) | the telling node's country / network |
| `stored` | a DHT torrent is stored | the node its hash was first heard from (`dht_seen.src_*`, set at triage; a re-queued row keeps its first one) |
| `nodes` | a node tells us about any hash, once per node and UTC day | that node |

Hashes triaged before D101 or with geo off have `src_cc` NULL and are not counted as stored
(no "unknown" inflation). Peers that delivered metadata are **not** counted (the fetcher does
not report which peer answered; not done).

### Nostr (`layer=nostr`)

Every 10 minutes a batch of up to 300 relays whose host was not resolved in the last 24 h is looked
up (DNS, IPv4 first, 5 s timeout, 16 at a time; `.onion`, `.i2p`, `.local` and failures =
unknown) and its country / network stored on the relay row. Metrics: `relays` (relays located
there; with `window=day` only those that answered in the last 24 h), `events` (new events first
delivered by those relays, `relays.events_found`) and `torrents` (new kind-2003 events,
`relays.torrents_found`, counted from D102 on). `window=all` uses the relays' lifetime counters
by their current location; `window=day` uses hourly counts from a snapshot of those counters
every 10 minutes (the first snapshot after a start is the baseline).

### Windows and storage

The tally lives in memory and is written every minute (and at stop) into `geo_counts_hourly`
(hour of the flush; kept 49 h) and `geo_counts_total` (since `meta.crawl_map_since`). `day` =
the last 24 hourly buckets, `all` = the totals. A crash loses at most a minute of counts.
Answers are cached 60 s.

### Crawl workers (D105)

A worker with `MAGNETAR_GEOIP` on downloads the databases into its own state dir and resolves
locally: each triage item carries `cc` / `asn` (`""` = resolved, not in the database), and the
request carries `geo_nodes` (its node counts since the last request) and `as_names` (AS names
it has not sent yet). The server counts `hashes` and `nodes` from those, stores the codes in
`dht_seen`, and counts `stored` when the worker's upload is stored. New optional fields with
defaults: **the worker API stays 1** (an older server ignores them; an older worker sends
none, its hashes are simply not located). Memory: +~18 MB with `on`, +~8 MB with `country`;
the RPI (200 MB RSS budget) should use `MAGNETAR_GEOIP=country` (or `off`) — not measured on
the Pi itself.

## API

### `GET /api/v1/stats/sources` → `CrawlSources`

```json
{"dht": {"routes": [{"via": "sample", "seen": 5265879, "stored": 1009417},
                    {"via": "get_peers", "seen": 2299481, "stored": 143237},
                    {"via": "announce", "seen": 7179, "stored": 810}],
         "hourly": [{"bucket": 1793098800, "seen": {"sample": 1200, "get_peers": 700, "announce": 3},
                     "stored": {"sample": 300, "get_peers": 40, "announce": 1}}],
         "hourly_since": 1793000000},
 "nostr": {"by_source": [{"source": "nip66", "relays": 1185, "enabled": 900, "delivered": 63,
                          "events": 179549, "torrents": 0}],
           "top_relays": [{"url": "wss://relay.example", "source": "seed", "enabled": true,
                           "ok_24h": true, "events": 99150, "torrents": 0, "country": "DE",
                           "asn": 24940, "as_org": "Hetzner Online GmbH"}],
           "torrents_since": 1793000000},
 "machines": [{"name": "server", "kind": "server", "connected": true, "since": 1793000000,
               "dht_triaged": 120000, "dht_queued": 50000, "dht_metadata": 9000, "dht_stored": 8000,
               "dht_triaged_by_route": {"sample": 80000, "get_peers": 39000, "announce": 1000},
               "seed_checks": 30000, "stored_per_min": 80.5}],
 "computed_at": 1793100000}
```

- `routes[].seen` = distinct hashes first triaged through the route: rows still in `dht_seen`
  plus stored ones (which leave it); `stored` = `dht_torrents.discovered_via`. Whole-table counts
  (≈1 s on production's 6.4 M rows), cached 10 minutes, 20 s statement timeout. (The reference
  numbers of 2026-10-27 counted `dht_seen` only: sampled 4.0 M → 941 k, 23 %; this definition
  adds the stored rows back to `seen`.)
- `hourly`: the last 48 hours of the new `stats_series` counters `dht_seen_<via>` (new
  `dht_seen` rows, statement trigger) and `dht_stored_<via>` (new `dht_torrents` rows), also
  available from `/stats/timeseries`. They start at the migration (`hourly_since`).
- `machines`: the server first (its crawler's counters since it started; seed checks of its
  own batches), then every configured worker (the server's per-worker counters, D99).
  `stored_per_min` over the last ~10 minutes. Counters reset when the server restarts.

### `GET /api/v1/stats/map?layer=dht|nostr&window=day|all` → `CrawlMap`

Defaults `layer=dht`, `window=day`; anything else is 400. Fields: `available`, `reason`,
`database` (`country_build`, `asn_build`, name, licence, url), `attribution`, `metrics`
(`hashes, stored, nodes` / `relays, events, torrents`), `countries` (every country, `counts`
parallel to `metrics`, ordered by the first), `networks` (top 50 by the first metric, with
`as_org` and the country most of its counts came from), `unknown`, `totals`, `since`,
`computed_at`. `/health` lists `crawl_sources` and `crawl_map`.

## Live check (2026-10-27, branch build on a scratch database)

`serve` (debug build) on an empty scratch database, DHT aggressive on UDP 37891–37894, 4 nodes,
800 q/s, no UPnP, direct fiber line (`ip route get 1.1.1.1` → public source address; connect
time to 1.1.1.1 stayed at 14–15 ms), 630 s:

- Both databases downloaded on start (8 348 509 + 9 566 363 bytes, October 2026 builds) and
  loaded 1.8 s after start; the ~2 000 hashes triaged before that have no codes (not counted).
- DHT, last 24 h: **10 976 hashes, 1 064 stored, 674 nodes** located in 67 countries, 0 unknown;
  top countries NL 1 947 / 226 / 97, US 1 451 / 129 / 84, CN 1 322 / 147 / 71, RU 1 189 / 137 /
  87; top networks AS60781 LeaseWeb Netherlands (689 hashes, 78 stored), AS208959 SlashN, AS4134
  Chinanet, AS212238 Datacamp, AS9009 M247. 331 AS names recorded; the three geo tables used
  408 kB.
- Routes: sample 12 836 seen → 1 207 stored, get_peers 142 → 0, announce 1 → 0 (a fresh
  identity is hardly known yet); server machine 12 981 triaged, 1 207 stored, 126 stored/min,
  4 472 seed checks.
- Nostr: 12 seed relays located (first batch ran before the crawl discovered more relays; the
  batch now runs every 10 minutes), 3 455 events / 3 441 torrents, DE 1 891, CA 1 470 (nos.lol:
  Hetzner DE; relay.nostr.net: Cloudflare).
- Privacy: the only address in the whole server log is the `127.0.0.1` bind line; no stored text
  value in the scratch database and no saved API answer matched an IPv4 pattern.

## App

Stats → Charts → **Where discoveries come from** card and the **Crawl map** screen
(docs/APP.md §15). The world outline is Natural Earth 1:110m Admin 0 (public domain),
simplified and quantised by `android/tools/world_map.py` into `assets/world_110m.bin`
(32 KB); attribution "IP Geolocation by DB-IP" (link) and "Map: Natural Earth" stays visible on
the map screen.
