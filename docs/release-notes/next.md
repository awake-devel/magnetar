v0.2.0 (pre-release): everything new since v0.1.0 + the metadata enrichment merge (`main` at `8444084`). Server and app
must be updated together to get all of it, but each side keeps working with an older other side
(details under "Older servers and offline mode").

## What's new

### Instant search (ARCHITECTURE.md D43, docs/SEARCH.md)

- The server answers searches from an embedded Tantivy index instead of Postgres. On the
  production copy (~500 k infohashes) a search takes 4–35 ms, the empty query and one-letter
  queries included (Postgres took seconds). Typos in titles are forgiven ("interstelar"), the
  last word matches as a prefix from 3 letters, and hits whose title has every word come first.
- New torrents, seed checks and matched titles show up in search 1–2 s after they are stored.
- The app searches as you type (250 ms pause), shows the phone's cached results while the
  server answers, and shows "1,000+" when the count is only a lower bound.
- Postgres stays the source of truth: the index can be deleted at any time and is rebuilt.

### Filters: language, quality and more (D44, docs/FILTERS.md)

- Every release name is read for resolution, source (Remux, BluRay, WEB-DL…), video codec, HDR /
  Dolby Vision, bit depth, audio codecs and channels, audio and subtitle languages, the French
  tags (VFF, VFQ, VF2, TRUEFRENCH, MULTi, VOSTFR…), group, season/episode, year and editions.
  Hand-checked on 320 production names: languages 97.5 %, French tags 99.2 %, quality 98.1 %.
- Filter sheet in Search with a live count next to every choice (counts ignore the filter of
  their own field, so picking "French" still shows how many English releases there are). Also
  genre, movie/series/anime, minimum rating, year range and a "rating" sort.
- Title page: every release of one movie or series, grouped by season/episode, quality and
  language. The detail screen shows the parsed attributes.
- Saved searches keep the filters, and their notifications (server and phone) respect them.

### Seed checks at scale (D45, docs/LIVENESS.md)

- The server keeps seeders/leechers current for every torrent: newest first, then re-checks by
  age (6 h for recent torrents, up to 4 days for old ones, 7 days after three dead checks).
  What you open, save or see in results is checked first.
- About 29 k torrents an hour on a home line at 3 % of a core, polite to trackers (1 request
  per second per tracker, backoff on failures). DHT answers from node farms that invent peers
  are ignored, so fewer dead torrents look alive.
- Seeders/leechers history (90 days) on the detail page; a seed-check card on the Status
  screen (backlog, checks per hour, alive/dead). `magnetar-server liveness` runs batches by hand.
- Fix: the IMDb and anime enrichers no longer deadlock each other (D46), and the flood of
  "word is too long to be indexed" notices is gone from the logs.

### Stats, charts and graphs (D47, docs/STATS.md)

- New **Stats** tab (replaces Status; the old Status screen is its third section): breakdowns
  of audio languages, resolutions, sources, codecs, HDR, audio, genres, kinds, categories and
  years. Tap a value to filter every other chart by it, long-press to search it.
- Activity charts (torrents added/published, relays discovered, crawl events, seed checks,
  database size), per hour or day.
- Health card: index, crawler, relays, seed checks, enrichment coverage.
- Interactive network graphs: web of trust, relays and publishers, a title's releases, and a
  torrent's swarm (from its detail page).

### One title page per film (D48)

- A film or show whose releases carried only a TMDB id no longer has a second, rating-less
  title page next to its IMDb one (e.g. Dune (2021) was split 17 / 9 releases): the server reads
  the IMDb id from TMDB's details and merges the two. Existing duplicates are fixed by a
  background pass after the update (needs `MAGNETAR_TMDB_TOKEN`; ~33.6 k rows on production,
  one TMDB request each at `MAGNETAR_TMDB_RPS`, about half an hour at the default 20/s).

### Fixes from the first phone test

- Posters keep their 2:3 shape (they were squeezed and cropped).
- Filter sheet: chips stay in place while counts change (a value with no match is greyed
  out, not removed), closing the sheet with Back or a swipe applies the picks, and the
  categories show counts too.
- Stats: "Not checked yet" is shown apart from "No answer" in the seed-check health bar.
- Graphs: parts that are not linked to the rest stay close to it, so the view is no longer
  shrunk to fit a far-away island.
- Settings: "Get my key from Amber" fills the trust root from a NIP-55 signer (only the
  public key is asked for). A private key (`nsec1…`) pasted into the field is cleared at once.

### DHT crawler: design (stage 0, nothing runs yet; docs/DHT.md, D49–D58)

- The server will find torrents on the BitTorrent DHT itself (BEP 51 sampling, and in
  "aggressive" mode by listening on open UDP ports mapped with UPnP), download only their
  metadata from peers and verify it against the infohash. Off by default (`MAGNETAR_DHT`).
- DHT results will be mixed in with Nostr results on equal terms: they pass the trust threshold
  and carry a "DHT" badge instead of a trust star.
- Adult content will be hidden by default everywhere, Nostr torrents in the adult category
  included, with a switch to show it.
- A hard blocklist for illegal content will drop such torrents from the DHT and from Nostr
  alike, keeping only their infohash so they are never fetched again; it cannot be turned off.
- Design review: the blocklist will check every name a torrent carries (including the ones
  apps hide, such as padding files) and, for Nostr torrents, the description and hashtags too;
  torrents whose metadata repeats a field are refused. Adult content is hidden when either the
  publisher or the DHT classifier says so.

### DHT crawler: discovery (stage 1; docs/DHT.md §13, D59–D63)

- New `magnetar-server dht [--seconds N] [--mode polite|aggressive] [--dry-run]`: runs the DHT
  crawler for a bounded time and prints a JSON report (hashes found per minute, peers per hash,
  CPU, memory). With `MAGNETAR_DHT=polite|aggressive`, `serve` runs it in the background.
  Default stays off.
- Stage 1 finds infohashes (BEP 51 sampling and incoming DHT queries), skips those already known
  or blocked, keeps a persistent queue (`dht_seen`) and looks up peers for each. It does not yet
  download metadata, so nothing new appears in search.
- Database migration `20261101000000_dht.sql` adds the DHT tables and the `torrent_rows` view.
  It briefly locks `torrent_infohashes` (`lock_timeout` 10 s: a start fails loudly instead of
  hanging behind another session).
- Live measurement from the home connection (270 s, no port opened): 534 hashes/min given peers,
  77 % of them with at least one peer, 1.6 % of a core, 47 MB RAM.
- Review fixes (D63): the server's view of its own public IP now follows an ISP address change
  instead of possibly sticking to the old one; another DHT node can no longer hide a torrent by
  announcing a look-alike hash first; if the crawler cannot start (no network yet at boot), `serve`
  retries it after 30 s, then up to every 15 min, instead of leaving it off.

### DHT crawler: metadata fetch (stage 2; docs/DHT.md §14, D64–D65)

- The crawler now downloads each found torrent's metadata (name, files, sizes) from its peers
  over TCP (BEP 3/10/9) and checks it against the infohash (SHA-1, or SHA-256 for hybrid
  torrents). Private torrents, v2-only torrents, malformed or oversized metadata are recorded
  and not retried; other failures back off 1 h, 6 h, 24 h, then stop.
- **Nothing found on the DHT is stored or shown yet**: the illegal-content blocklist (next
  stage) must check every name first, and until it exists the server drops the verified
  metadata and keeps only the hash in its queue (fail closed).
- New settings: `MAGNETAR_DHT_FETCH_CONCURRENCY` (128 aggressive / 16 polite),
  `MAGNETAR_DHT_METADATA_MAX` (16 MiB), `MAGNETAR_DHT_TCP_RATE` (150 new connections/s) and
  `MAGNETAR_DHT_TCP_MAX` (350 open) — limits measured on the home router (D66) — and
  `MAGNETAR_DHT_PROBE`: a TCP connect to 1.1.1.1 / 9.9.9.9 every 5 s that halves the crawler's
  rates for 5 min when the router starts to struggle (`off` disables it). The `dht` report adds
  fetch counts, verified metadata per minute, success rate, bandwidth and the probe.
- Live measurement from the home connection (7.5 min, no port opened, D66 limits): 95 torrents'
  metadata verified per minute (58 at the first, 25 connections/s limit), 26 % of fetch
  attempts succeeded (18 % before; most DHT peers no longer accept
  connections), 1 Mbit/s down, 2 % of a core, 64 MB RAM. Tested against a real BitTorrent
  client (librqbit) and 16 kinds of broken or hostile peers.
- Review fixes (D67): metadata built to blow up memory when decoded (millions of tiny values)
  is refused before decoding; peers that flood unused extension messages are dropped like
  other floods; the content screen now sees every whole file path of v2 file trees; database
  errors in the crawler log only their error code, never a message that could quote a name.

### DHT crawler: classification, adult filter, illegal-content blocklist (stage 3; docs/DHT.md §15, D68–D70)

- **Adult content is hidden by default, everywhere**: search, browse, filter and Stats counts,
  saved-search notifications, on the server and on the phone. Settings → "Show adult content"
  turns it back on; picking the adult category in the filter sheet shows it for that search.
  This also hides the Nostr torrents publishers filed as adult (36 809 infohashes on
  production). API: `adult=1` / `include_adult`. Older apps get the default (hidden).
- The crawler now **stores** what it finds: each torrent is classified from its name and files
  (video, anime, audio, book, application, game, picture, adult, other) and gets release
  attributes, seed checks and IMDb/anime title links like Nostr torrents. Hand-checked on 100
  fresh names: 83 % in the right category; adult tagging made no false positive and caught 2
  of 3 adult torrents (names with only performer names or Chinese voyeur words are missed).
  DHT torrents appear in search results with the next stage.
- **Hard illegal-content blocklist**, always on, for the DHT and for Nostr events (title, file
  names, description, hashtags): a match keeps only the infohash (and the event id), never a
  name, so it is never fetched or accepted again; relays get `blocked: content policy`. Names in
  Chinese/Japanese/Korean are checked too, and a name labelled with a child's age is blocked
  whatever else it says. The list ships as hashes only; `MAGNETAR_BLOCKLIST_FILE` can add terms
  (never remove), and a file that does not load stops the server.
- Live measurement (2 × 7 min, home connection, no port opened): 96–117 torrents verified per
  minute, 1 477 stored, 16 blocked by the list (counted, never looked at), 2–3 % of a core,
  ≤ 107 MB RAM. One stored picture set labelled with a child's age slipped through the first
  version; it was removed by hand and is why such labels now block on their own.
- The blocklist also sees through disguised spellings: invisible characters inside a word,
  accents typed as separate marks, fullwidth or decorated letters, Cyrillic/Greek look-alike
  letters, and words written with dots or spaces between the letters. Very large torrent
  descriptions no longer cost the crawler a lot of memory to check.

## Older servers and offline mode

- The app asks `GET /health` for `features` and hides what the server lacks: an older server
  still works, without filters, title pages, history, Stats or graphs.
- Offline (phone cache only) the app still filters by the parsed attributes it can compute
  locally (language, resolution, source, codecs…), not by genre, kind or rating; the Stats tab
  shows counts of the phone's own cache without cross-filtering.
- An older app talking to the new server keeps working: all new fields have defaults. It gets
  adult content hidden (the new default) and has no switch to show it.
- The new app talking to an older server drops adult results itself (that server ignores the
  setting).

## Upgrade notes

Measured on a full copy of the production database (501 528 infohashes, 506 171 events,
7.3 GB) on this laptop, 2026-10-26:

| Step | When | Duration |
|---|---|---|
| Five new database migrations | before the server listens | 13.5 s in total (11.9 s for the seed-check schedule, 1.4 s for the search counters) |
| Search index build | background, after the server listens | 35 s, 389 MB on disk |
| Parsed release attributes backfill | background, at the same time | 117 s |
| Activity history backfill (`torrents_added`) | background | 2 s |
| Next start | | index reused, ready in 0.6 s |
| Search index rebuild for the adult flag (`SCHEMA_VERSION` 3, stage 3) | background, once | not measured; the version 2 build took 35 s on the same copy |

- The API answers during the build: searches use Postgres until the index is ready (slow on
  big queries, as before; title pages too), and the filter counts and Stats breakdowns return
  503 until then. Filters on release attributes give incomplete results until the backfill ends.
- Seed-check backlog: ~450 k torrents were never checked; at ~29 k/h that is about 16 h after
  the upgrade, newest first. Lower `MAGNETAR_LIVENESS_DHT_CONCURRENCY` or
  `MAGNETAR_LIVENESS_TRACKER_RPS` on a weak connection.
- Docker: the index lives in the existing data volume (`/data/state/magnetar/search-index`);
  nothing to add. New optional settings are listed in docs/CONFIG.md (`MAGNETAR_SEARCH_INDEX*`,
  `MAGNETAR_LIVENESS_*`); the defaults are fine. `compose.yml` has commented CPU/memory limits.
- The phone's cache gets a new table and parses its torrents in the background after the app
  update; nothing to do.
- Run one server per database and per index directory (two would consume each other's index
  change log).

## Known limitations

- The new app screens (filter sheet, title page, Stats tab, graphs, history chart) were built
  and unit-tested, but not tried on a phone or an emulator in this cycle.
- Activity history before the upgrade only exists for "torrents added", and dates are when the
  crawler first saw a torrent: the first crawl shows as one huge day. Seed checks, relays,
  crawl events and database size start empty.
- Seeder counts of an old torrent can be up to 4 days old (7 days if dead). DHT peer counts
  are still estimates, and node farms spread over many IPs can still fool them.
- The swarm graph shows which trackers answered, not seeders per tracker (not stored). The
  relays graph shows the relays publishers say they write to, not the relay that delivered
  each torrent.
- The release parser reads names only (plus the largest video file); a mislabelled release is
  filtered by its label. A title page lists at most 1000 releases (best seeded first); for the
  biggest titles that answer is about 900 KB.
- In the server log, the first start prints two migration statements in full as "slow
  statement" warnings, and the first index build logged three Tantivy "couldn't find segment"
  warnings while the attribute backfill re-indexed documents; the finished index was complete
  (501 528 documents, nothing pending).
