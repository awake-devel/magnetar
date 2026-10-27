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

### Torrents found on the BitTorrent DHT (docs/DHT.md, ARCHITECTURE.md D49–D75)

The server can now find torrents by itself on the BitTorrent DHT, the network every
BitTorrent client uses to find peers, instead of only receiving them from Nostr publishers.
It is **off by default**: while it runs, your IP address takes part in the DHT and is visible to
the peers it contacts, so turn it on only if you accept that.

**What you get**

- **More results, mixed in equally.** Torrents found on the DHT appear in search, browse,
  filters, title pages, Stats and saved-search notifications next to the Nostr ones. There is
  no publisher to trust for them, so they are never hidden by the trust setting: the server
  only keeps a DHT torrent after downloading its name and file list from other peers and
  checking them against the torrent's own hash. They show a neutral **DHT** badge where Nostr
  torrents show the trust star; a torrent known from both shows the star and a small DHT chip.
- **Source filter.** The filter sheet has Source chips (Nostr / DHT) with counts. A DHT torrent's
  page says when it was found and how many peers had it, in place of the publisher card.
- **Adult content is hidden by default, everywhere**, for DHT and Nostr torrents alike (this
  also hides the 36 809 torrents Nostr publishers filed as adult). Settings → "Show adult
  content" turns it back on; picking the adult category in the filter sheet shows it for that
  search. Older apps get the default (hidden).
- **Illegal content is blocked for good.** A built-in blocklist checks every name a torrent
  carries (title, every file name, and for Nostr torrents also the description and tags),
  including disguised spellings and Chinese, Japanese and Korean names. A match is dropped
  entirely: only the hash is kept so it is never fetched or accepted again, never its name.
  It cannot be turned off; `MAGNETAR_BLOCKLIST_FILE` can only add terms, and a list that fails
  to load stops the server.
- **Crawler status.** Stats → health card and the Status screen show the crawler's numbers
  (new hashes per minute, queue, torrents stored, adult, blocked, router ports); the activity
  chart has "DHT torrents added" and "DHT hashes found".

**Turning it on**

- Set `MAGNETAR_DHT=aggressive` (or `polite` for a lighter crawl: one node, a quarter of the
  DHT queries, and it neither answers other nodes nor listens for their announcements) and
  restart the server. With Docker: `docker compose -f compose.yml -f docker/compose.dht.yml up -d` (docs/DOCKER.md).
- In aggressive mode the server asks your router (UPnP) to open UDP ports 6891–6894 while it
  runs and closes them when it stops; `MAGNETAR_DHT_UPNP=false` turns that off. Nothing listens
  on TCP. In Docker, set `MAGNETAR_DHT_UPNP_GATEWAY` and `MAGNETAR_DHT_UPNP_LOCAL_IP` for UPnP
  to work. All settings are in docs/CONFIG.md.
- It watches your connection: when the router starts to struggle it halves its own speed for
  5 minutes (`MAGNETAR_DHT_PROBE=off` disables that).

**What to expect** (measured on a copy of the production database, home connection, aggressive
mode, no router port opened, 10 minutes, 2026-10-26)

- About **110 new torrents a minute** stored once running (1 146 in 10 minutes: 537 video,
  193 adult, 131 audio, 96 books, the rest apps, games, anime and pictures); 6 dropped by the
  blocklist. 31 % of metadata downloads succeed: most DHT peers do not accept connections.
- **Disk: no limit, by your choice.** About 10 KB of database per stored torrent (the queue of
  found-but-not-yet-fetched hashes included) plus ~3 KB of search index: at this rate roughly
  2 GB a day. The Stats tab shows the database size.
- **Search stays fast**: 9–11 ms typical and 18–23 ms for the slowest 5 % of searches while
  crawling, about the same as without it (18 ms).
- The crawler's own load is small (2–3 % of a core, ~100 MB in the crawl-only runs); during
  this run the whole server, including the one-off index rebuild below and seed checks, used
  37 % of a core and at most 850 MB of memory.

**Upgrade**

- One database migration (48 ms on the production copy) and a one-off search index rebuild at
  the first start (132 s for 502 k torrents on the production copy while the crawler and seed
  checks ran; searches use Postgres meanwhile). Both happen whether or not the crawler is on.
  The next start reuses the index (ready in under 1 s).

### App rebuild, stage 1: server (D76–D78)

- **Browse is no longer flooded by unchecked DHT torrents.** The default list (no search text,
  no Source chip) shows a torrent found only on the DHT once a seed check found peers. Before,
  the first 50 results on production were all DHT torrents with unknown peers. Searching any
  text, or tapping the **DHT** chip, still lists every DHT result. Nostr results are unchanged.
  Adult detection is unchanged too (no stricter rules, by your choice); adult content stays
  hidden by default and is never deleted.
- **Everything the app needs is in the REST API** (docs/API.md), so the new app can be a plain
  Kotlin client: saved searches can be read, edited (`PUT`) and polled for new results from the
  server (`GET /api/v1/saved-searches/poll?since=…`; the phone keeps its own position, so it
  never steals the server's ntfy notifications). Results now carry `best_peers` (tracker
  seeders or DHT peers, whichever is higher), so a DHT torrent with only DHT peers no longer
  reads as "unknown". "Check now" on a DHT-only torrent works (it answered "not found").
- Editing a saved search while the server checks it for notifications no longer risks the edit
  being undone: the notification check now saves only its own progress (review fix).
- **Faster test builds**: each crate's integration tests are one test program instead of one
  per file (46 test executables → 10, `target/` 24 GB → 9.9 GB). Measured on this laptop with
  mold, each tree from scratch in its own target directory, `cargo test --workspace`:

  | | before | after |
  |---|---|---|
  | Build from scratch (`--no-run`) | 371 s | 422 s |
  | Run (everything built; doc-tests compile here) | 381 s / 443 s | 223 s / 177 s |
  | Rebuild after touching `crates/server/src` | 92 s | 38 s |
  | Rebuild after touching `crates/core/src` | 120 s | 54 s |
  | Rebuild after touching one server test file | 7.9 s | 16.0 s |

  So an edit-and-test cycle on the code is ~2.5 × faster (server change + run: 535 s → 215 s);
  a from-scratch build is 14 % slower (one big test crate compiles with less parallelism at
  the end), and editing only a test file costs 8 s more.
- Upgrade: no migration and no index rebuild (the browse rule reads existing index fields).

### App rebuild, stage 2: app design and skeleton (D79–D84, docs/APP.md)

- **The Android app is being rebuilt as a plain client of your server**: Kotlin and Jetpack
  Compose only, no Rust library inside any more. It keeps search (instant results, filters
  with live counts, sort, detail, title page), saved searches with hourly notifications
  through the server, the Stats tab with its charts and four graphs, and Settings. The design,
  every screen's API calls, the cache, timeouts, theming and accessibility are in docs/APP.md.
- **Gone from the app**: downloads (on the phone and the server-download screen; *Open magnet*
  and *Copy magnet* hand the torrent to your torrent app), the phone crawler and the offline
  search database. A small cache of recent pages stays for instant display and offline
  viewing (search stage).
- **Your settings carry over**: same package name and signing key, so the new build installs
  over the old one. The server URL, token, trust root, trust threshold and the adult switch are
  read from the old app's settings; the old "background sync" switch becomes the saved-search
  notification switch. The old background crawl job and the "Downloads" notification channel
  are removed; the "Saved search alerts" channel keeps its id, so its sound settings stay. Old
  saved searches (kept only on the phone before) are moved to the server in the saved-search
  stage.
- Adult content stays hidden by default with a Settings switch to show it; the app adds no
  adult detection of its own (no stricter rules).
- This stage is the skeleton only: bottom bar (Search, Saved, Stats, Settings), theme (unchanged
  dark theme and chart colours), the API client and the settings store. The tabs are
  placeholders; Settings shows the migrated server and has *Test connection*.
- **Build**: plain Gradle (`scripts/build-android.sh` no longer needs Rust, cargo-ndk or the
  NDK, and runs the unit tests). `crates/ffi` and `crates/uniffi-bindgen` are deleted from the
  workspace. New libraries, versions and licences checked on Maven Central: OkHttp 5.5.1,
  kotlinx.serialization 1.12.0 (Apache-2.0); unit tests run on TestNG 7.12.0 (Apache-2.0)
  instead of JUnit 4 (EPL-1.0, not on the allowed licence list).
- Review fixes: a seed check, a Stats answer or the saved-search poll is no longer cut after
  15 s by the read timeout (each request class now waits up to its own limit, 45 s for a seed
  check); an API token pasted with a line break or a non-ASCII character now gives a clear
  "token cannot be sent" error instead of crashing the app; the settings migration writes to
  disk in the background instead of on app start.

### App rebuild, stage 3: search side (D85–D87, docs/APP.md §12)

- **Search** searches as you type (250 ms pause); a newer keystroke cancels the request in
  flight, so an old answer never replaces a newer one. The last answer for the same search is
  shown at once from a small cache ("cached · 5 min ago") while the server answers, and stays
  with an "Offline" banner when the server cannot be reached. Rows show the title line
  ("Dune (2021) · ★ 8.0 ›", tap for the title page), size, seeders (or the DHT estimate),
  age, release attributes, the file the text matched in, and the Nostr / DHT badge.
  "1,000+" when the server only knows a lower bound.
- **Filter sheet** with live counts from the server next to every choice: categories, Source
  (Nostr / DHT), audio and subtitle languages, resolution, release source, codec, HDR, audio,
  genre, type, minimum rating, years, size, publication date, seeders, dead torrents, trust.
  Chips keep their place while you pick (a choice that would give nothing stays, greyed);
  Back or swiping the sheet down applies it like *Show*. Sort by relevance, date, size,
  seeders or rating, either direction.
- **Torrent detail**: poster, overview, rating and genres of the film or series with *All
  releases*; release attributes; folders and files; swarm with *Check now* and a 90-day seeder
  chart; trust (or "Found on the DHT"); *Open magnet* / *Copy magnet*. No download buttons.
- **Title page**: every release of a film or series grouped by season/episode, quality and
  language, with chips to narrow them; the search's filters apply ("Show all" removes them).
- **Saved searches** (brought forward from the next stage): save the current search with its
  filters from the star, run, delete, and switch notifications per search; an hourly
  background check asks the server for new hits and posts one notification per search, which
  opens the newest torrent. Needs a server with `saved_search_poll` (stage 1) for the
  notification switch and the alerts; an older server still lists, creates and deletes. The
  old app's saved searches (kept only on the phone) are moved to the server once, and the old
  phone database is deleted only after all of them are there.
- Adult content: unchanged, hidden by default, the Settings switch is sent with every search
  and the app adds no detection of its own.
- Measured on the Android emulator against the production server (read-only): typing to
  results 0.33–0.85 s including the 250 ms pause (median ≈ 0.59 s over 12 queries; one cold
  query took 3.4 s, 2.9 s of it on the server); first results 3.4–4.1 s after a cold start
  of the debug build on the software-rendered emulator.
- No new library: posters use the app's HTTP client and its cache instead of Coil (D85). UI
  tests run on the emulator from `android/uitest/uitest.py` (D86).
- Review fixes (D88): offline, the cached page shows at once instead of after the server
  check times out; scrolling to the end of a list that was first shown from the cache loads
  the next page again; after switching servers, the new server's features are used for its
  first search; *All releases* is only offered by servers that have title pages; old saved
  searches without a name are not uploaded twice when the move is retried.

### App rebuild, stage 4: stats, graphs and settings (D89–D91, docs/APP.md §13)

- **Stats → Charts**: a health card (seed checks as seeded / dead / no answer, with *not
  checked yet* on its own line; index, relays, linked titles, last crawl, and the DHT
  crawler's discovery, queue, metadata, stored and blocked counts), breakdown donuts and bars
  (category, languages, resolution, movie/series/anime, release source, codec, HDR, audio,
  genre, publisher categories, Nostr / DHT) and a year histogram. Tap a value to filter every
  other chart by it; long-press to open Search with it and the active filters. Activity over
  days or hours, DHT series included; pinch to zoom, drag sideways, tap for a value.
- **Stats → Graphs**: web of trust and relays; the releases graph of a title (title page,
  top bar) and the swarm graph of a torrent (detail, swarm card). The graph is fitted to the
  screen and keeps detached groups and lone nodes next to the main cluster (D90); pinch,
  drag, double-tap to fit, tap a node for its card, or switch to the node *List*.
- **Stats → Status**: server version and the features it lacks, search index, seed-check
  backlog (due now, time to clear it, checks per hour, last batch), relays, DHT crawler.
- **Settings**: server URL with the plain-HTTP warning, API token (Show/Hide), *Test
  connection* (before saving); trust root with *Get my key from Amber*, an nsec is refused and
  cleared at once even when typed one character at a time; trust threshold, sent to the server
  on *Save* (`PUT /api/v1/settings/trust`, only when it differs; an empty field never wipes the
  server's root unless you emptied it); *Show adult content* and *Saved-search alerts*
  switches (apply at once); notification permission; TMDB / IMDb attribution; versions.
- Adult content: unchanged, hidden by default; the switch is sent with every chart request;
  the app adds no detection of its own.
- No new library: the charts are drawn by the app (D89); Vico was checked (3.3.1, Apache-2.0)
  and not needed. Stats answers are cached like search pages and shown offline with their age.

## Older servers and offline mode

- The app asks `GET /health` for `features` and hides what the server lacks: an older server
  still works, without filters, title pages, history, Stats or graphs.
- Offline the rebuilt app (D79) shows the last cached answers (search pages, details, title
  pages, Stats) with their age; it has no local index, so nothing new can be searched or
  counted offline.
- An older app talking to the new server keeps working: all new fields have defaults. It gets
  adult content hidden (the new default) and has no switch to show it.
- The new app talking to an older server drops adult results itself (that server ignores the
  setting). It hides the Source chips and DHT charts when the server does not list `origin` /
  `dht`.

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
| DHT migration (sixth, `20261101000000_dht.sql`; later copy: 501 854 infohashes, 8.0 GB) | before the server listens | 48 ms |
| Search index rebuild for the adult flag and DHT torrents (`SCHEMA_VERSION` 4) | background, once | 132 s from an empty index directory with the crawler and seed checks running (35 s for the version 2 build without them) |

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

- DHT crawler: opening router ports with UPnP was tested only against a simulated router, not a
  real one and not from a container, so whether other nodes reach the server through mapped ports
  (and how much more it finds that way) is not measured. The DHT screens of the app were built
  and unit-tested, not tried on a phone. Adult tagging is not perfect: in a hand check of 100
  names it caught 2 of the 3 adult torrents (names made only of performer names, or Chinese
  voyeur words, are missed) and made no false positive.
- During the first minutes after a start with the crawler on, while the index is rebuilt, the
  log shows about a hundred "slow statement" and "slow acquire" warnings from the database
  (114 during the 2-minute build on the production copy, then none until 4 at shutdown).
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
