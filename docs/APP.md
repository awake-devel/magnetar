# The Android app: a pure server frontend (D79–D84)

The app was rebuilt from scratch in 2026-10 (branch `app/rebuild`). It is a thin Kotlin client
of the server's REST API (docs/API.md) and nothing else: no Rust, no uniffi, no JNA, no phone
crawler, no local search engine, no downloads. It keeps the package name `app.magnetar` and
the release key in `android/keystore/`, so it installs over the old app and keeps the user's
settings (§7).

## 1. What the app does (and does not)

| Tab | Content |
|---|---|
| **Search** | Search box with instant results (search as you type); filter sheet with live facet counts (category, languages, subtitles, resolution, source, codec, HDR, audio, genre, movie/series/anime, rating, years, **Source Nostr / DHT**), sort; result list (title, media line "Title (year) · rating", attributes, size, seeders, source badge Nostr / DHT / both); torrent detail (media card with poster, French overview, release attributes, files, swarm with **Check now** and the seeder history chart, trust, *Open magnet* / *Copy magnet*); title page grouping the releases of one movie/series |
| **Saved** | Saved searches (stored on the server), run one, edit, delete, per-search "notify"; hourly notifications through the server's stateless poll |
| **Stats** | Health card, breakdown charts with cross-filtering, activity series, the four network graphs: web of trust, relays and publishers, title releases (from a title page), swarm (from a detail page) |
| **Settings** | Server URL + API token + *Test connection*; trust root with **Get my key from Amber** (NIP-55 `get_public_key`) and nsec rejection; trust threshold (pushed to the server); *Show adult content*; saved-search notifications on/off; TMDB / IMDb attribution; version |

Not in the app, by decision (D79):

- **No downloads**, neither on the phone (librqbit is gone with the ffi) nor a UI for the
  server's downloads. *Open magnet* hands `TorrentDetail.magnet` to an external torrent app
  (`Intent.ACTION_VIEW`, `ActivityNotFoundException` → copy instead); *Copy magnet* copies it.
- No phone crawler, sync, or SQLite search engine. The only local data are the settings and a
  small cache of recent pages for instant display and offline viewing (§5).
- No Google services or Play libraries, no Firebase, no analytics, no voice input.
- No adult detection of its own: the server classifies (D52, D69), the app only sends the
  user's switch (§6). The adult rules are not made stricter by the client.

## 2. Architecture

- **Kotlin + Jetpack Compose + Material 3**, single activity, `navigation-compose`.
- **Coroutines / Flow**: one `ViewModel` per screen exposing a `StateFlow<UiState>`; the
  search box feeds a `MutableStateFlow<String>` that is `debounce(250 ms)` + `mapLatest`, so a
  new keystroke cancels the request in flight (§4).
- **HTTP: OkHttp**, **JSON: kotlinx.serialization** (compiler plugin), both behind one class,
  `api.MagnetarApi`. No Retrofit (one more library for ~25 endpoints that are easier to read
  as plain functions).
- **Images: no image library (D85).** Posters are one image per detail / title page; they are
  fetched with the app's own `OkHttpClient` (bearer header, same pool) plus an OkHttp HTTP disk
  cache (`cacheDir/posters`, 30 MB), which honours the server's
  `Cache-Control: private, max-age=604800` + `ETag`, decoded with `BitmapFactory` (down-sampled
  to ≤ ~600 px high) and kept in a 16 MB memory LRU. Coil 3.6.3 was planned (D80) but pulls
  `accompanist-drawablepainter`, a JetBrains Compose foundation build and OkHttp 4 for ~60
  lines of work.
- **Charts: Compose `Canvas`, no chart library (D89).** The activity series, the breakdown
  bars/donuts, the year histogram and the graphs are all drawn with `Canvas` (own force layout,
  no graph library). Vico 3.3.1 (Apache-2.0) was checked but not re-added (§13).
- **Background: WorkManager** for the hourly saved-search poll (§8). No foreground service.
- **Settings: `SharedPreferences`** file `settings` (the old app's file, §7), wrapped by
  `settings.SettingsStore` exposing a `StateFlow<AppSettings>`. No DataStore: it would need a
  migration of its own and buys nothing for eight values.

### Libraries (D80) — versions and licences checked on Maven Central, 2026-10-27

| Library | Version | Licence (POM) | Use |
|---|---|---|---|
| `com.squareup.okhttp3:okhttp` | 5.5.1 | Apache-2.0 | HTTP client |
| `org.jetbrains.kotlinx:kotlinx-serialization-json` | 1.12.0 (built with Kotlin 2.4.10) | Apache-2.0 | JSON |
| `org.jetbrains.kotlin.plugin.serialization` (Gradle plugin) | 2.4.21 (= the Kotlin / compose plugin version) | Apache-2.0 | compiler plugin |
| `org.jetbrains.kotlinx:kotlinx-coroutines-android` | 1.11.0 | Apache-2.0 | coroutines (kept) |
| ~~`io.coil-kt.coil3:coil-compose` + `coil-network-okhttp`~~ | 3.6.3 | Apache-2.0 | **not added** (D85): posters use OkHttp's cache + `BitmapFactory` |
| ~~`com.patrykandpatrick.vico:compose-m3`~~ | 3.3.1 (POM: Apache-2.0; deps `vico:compose` 3.3.1, `material3` 1.9.0, Kotlin stdlib 2.4.10) | Apache-2.0 | **not added** (D89): the series are drawn with Compose `Canvas` |
| AndroidX: compose BOM 2026.09.00, material3, material-icons-core, core-ktx 1.19.1, activity-compose 1.13.0, lifecycle-*-compose 2.11.0, navigation-compose 2.10.2, work-runtime-ktx 2.12.0 | (kept) | Apache-2.0 | UI, navigation, background work |
| test: `org.testng:testng` | 7.12.0 | Apache-2.0 (deps: jcommander 1.83 Apache-2.0, slf4j-api 2.0.16 MIT, webjars jquery 3.7.1 MIT; guice is optional, not pulled) | unit test runner, `useTestNG()` |
| test: `com.squareup.okhttp3:mockwebserver3` | 5.5.1 | Apache-2.0 | API client tests |
| test: `org.jetbrains.kotlinx:kotlinx-coroutines-test` | 1.11.0 | Apache-2.0 | `runTest` |

Removed: `net.java.dev.jna:jna` (the ffi is gone) and `junit:junit` 4.13.2, whose POM says Eclipse
Public License 1.0 — not in the allowed licence list, so the unit tests run on TestNG. Every addition in a later stage is checked
the same way (version and licence on its Maven Central page) and listed here.

## 3. Package layout

```
android/app/src/main/java/app/magnetar/
  MagnetarApp.kt          Application: settings store, API client, channels, legacy cleanup
  MainActivity.kt         single activity, deep link magnetar://torrent/{infohash}
  Notifications.kt        channel "saved_searches" (same id as the old app)
  api/
    MagnetarApi.kt        one suspend fun per endpoint, auth, error mapping
    Models.kt             @Serializable mirrors of magnetar_core::{api,model,query} types
    ApiException.kt       sealed error type the UI renders
    Http.kt               shared OkHttpClient, timeouts per request class, Call.await()
  settings/
    AppSettings.kt        the values + checks (nsec rejection, URL normalisation)
    SettingsStore.kt      SharedPreferences "settings" + migration from the old app
  cache/PageCache.kt      recent pages and details on disk (§5)
  data/Repository.kt      settings + service + /health features + cache, shared by the screens
  api/MagnetarService.kt  the interface the view models call (a fake in the unit tests)
  work/                   SavedSearchPollWorker, SavedSearchPolling (schedule), LegacyImport, PollLogic
  ui/
    AppShell.kt           Scaffold + bottom NavigationBar (Search, Saved, Stats, Settings) + NavHost
    theme/                MagnetarTheme, ChartColors (unchanged from the old app)
    search/ detail/ title/ saved/ stats/ graph/ settings/ common/
android/app/src/test/java/app/magnetar/   JVM unit tests (no emulator needed)
android/uitest/           emulator UI test over adb (uitest.py) + its fake server (§12)
```

Routes: `search`, `saved`, `stats`, `settings` (tabs); `torrent/{infohash}`, `title/{mediaId}`,
`graph?path=` pushed on top of the current tab. The deep link `magnetar://torrent/{infohash}`
(saved-search notifications) opens the detail.

## 4. API contract, errors and timeouts

### Screen → endpoint

| Screen / action | Endpoint(s) | Feature in `/health` |
|---|---|---|
| App start, Settings *Test connection* | `GET /health` (no auth) then `GET /api/v1/settings/trust` (checks the token) | — |
| Search list, browse (empty box) | `POST /api/v1/search` (`SearchQuery`, `limit` 50, `offset` paging) | `filters` for the release filters; `origin` for the source chip and badge |
| Filter sheet counts | `POST /api/v1/facets` `{query, fields, size}` (disjunctive) | `facet_set` |
| Sort | `SearchQuery.sort` + `descending` (`rating` needs `filters`) | — |
| Torrent detail | `GET /api/v1/torrents/{infohash}` (`TorrentDetail`: magnet, files, media + poster path, trust factors, DHT info) | — |
| Poster | `GET /api/v1/media/{id}/poster` (bearer, ETag) | — |
| Check now | `POST /api/v1/torrents/{infohash}/liveness` | — |
| Seeder history chart | `GET /api/v1/torrents/{infohash}/liveness-history?days=90` | `liveness_history` |
| Title page | `POST /api/v1/media/{id}/releases` (`SearchQuery` with the active filters) | `media_releases` |
| Title releases graph / swarm graph | `GET /api/v1/graphs/media/{id}`, `GET /api/v1/graphs/swarm/{infohash}` | `graphs` |
| Saved list / edit / delete | `GET /api/v1/saved-searches`, `POST`, `PUT /{id}`, `DELETE /{id}` | `PUT`/`GET /{id}`: `saved_search_poll` |
| Notifications (worker) | `GET /api/v1/saved-searches/poll?since=…[&adult=1]` | `saved_search_poll` |
| Stats health | `GET /api/v1/stats/health` | `stats` |
| Stats breakdowns (cross-filter) | `POST /api/v1/stats/breakdown` `{query, fields, top}` | `stats` |
| Stats activity | `GET /api/v1/stats/timeseries?metric=…&bucket=hour\|day&since=` | `stats` |
| Web of trust / relays graphs | `GET /api/v1/graphs/wot`, `GET /api/v1/graphs/relays` | `graphs` |
| Settings trust root + threshold | `GET` / `PUT /api/v1/settings/trust` `{root, threshold}` | — |

A query that uses a feature-gated field (release filters, rating sort, source chips) is never
sent to a server that lacks the feature: it would ignore the field and answer unfiltered
(docs/FILTERS.md). A feature the server does not list is hidden, with a one-line "needs a newer server" where a
whole section disappears. Production on 2026-10-27 (v0.2.0 before stage 1) lists `filters,
facet_set, media_releases, liveness_history, stats, graphs, adult_filter, origin, dht`, so
until it is updated the Saved tab still lists/creates/deletes searches but notifications show
"needs a newer server".

### JSON rules

- `Json { ignoreUnknownKeys = true; coerceInputValues = true; explicitNulls = false;
  encodeDefaults = true }`: newer server fields are ignored, an unknown enum value (a future
  category) becomes the field's default instead of failing the page, nulls are not sent.
- Field names are the Rust names (snake_case) via `@SerialName`; Kotlin names are camelCase.
- `u64` → `Long`, `u8`/`u32` → `Int`, `f32` → `Float`; infohashes and keys are `String`.

### Auth

`Authorization: Bearer <token>` is added by an interceptor **only to requests whose scheme,
host and port are the configured server's**, so a poster or a redirect to another host never
receives the token (OkHttp also drops it on a redirect to another host). The token is never
logged: there is no logging interceptor. A token with a character that cannot go into a header
(a pasted line break, a non-ASCII character) fails the call as `Unauthorized` with a message
saying so, before anything is sent; OkHttp would otherwise throw on its dispatcher thread and
kill the app.

### Timeouts and cancellation (search must never hang the UI)

All calls share one `OkHttpClient` (connect 5 s, read 15 s, write 15 s); each request class
then sets a **call timeout** (the whole request, DNS to last byte) through `newBuilder()`,
which shares the pool and dispatcher. The class also raises the read timeout to its call
timeout, since the server sends nothing until a seed check or poll is done and a 15 s read
timeout would cut a 45 s check:

| Class | Call timeout | Endpoints |
|---|---|---|
| `SEARCH` | 10 s | search, facets, releases |
| `DETAIL` | 15 s | detail, liveness history, trust settings, saved searches, health |
| `CHECK` | 45 s | check now (trackers + DHT on the server) |
| `STATS` | 30 s | stats, graphs |
| `POLL` | 60 s | saved-search poll (worker only) |

`suspend` functions wrap `Call.enqueue` in `suspendCancellableCoroutine` and `cancel()` the
OkHttp call when the coroutine is cancelled, so a stale search (a newer keystroke, leaving the
screen) frees its connection at once. All network work runs off the main thread (OkHttp's
dispatcher); JSON decoding runs on `Dispatchers.Default`.

### Errors

`ApiException` (sealed) is what screens render:

| Case | Type | UI |
|---|---|---|
| No server URL set | `NotConfigured` | "Set your server in Settings" + button |
| 401 | `Unauthorized` | "Token rejected" → Settings |
| 404 | `NotFound` | "Not on this server" |
| 503 `search_index_unavailable` | `IndexBuilding` | "The server is building its search index" + retry; cached page if any |
| other 4xx/5xx with `{"error","message"}` | `Http(status, code, message)` | the server's message |
| call/connect/read timeout | `Timeout` | "The server did not answer in time" + retry; cached page if any |
| other `IOException` | `Network` | "Server unreachable" + retry; cached page if any |
| bad JSON | `Decode` | "Unexpected answer from the server" |

Coroutine cancellation is never turned into an error.

## 5. Cache (D83)

`cache.PageCache` (search stage): the last pages and details as the server's JSON, in
`cacheDir/pages/`, keyed by SHA-256 of (server URL, endpoint, canonical request body incl. the
adult switch). Capped at 20 MB / 500 entries, least recently used first out; Android may also
clear `cacheDir`. Use: show the cached answer immediately (marked "cached · 5 min ago"), then
replace it with the server's; offline, the cached answer stays with an "Offline" banner.
Only search pages (first page), details, title pages and Stats answers are cached; posters are
in OkHttp's HTTP cache (`cacheDir/posters`, D85). Changing the server URL or token clears the cache. Nothing in the
cache comes from anywhere but the server's answers, so it never holds what the server filtered
out (the illegal-content blocklist works on the server, docs/DHT.md §7).

## 6. Adult switch

Off by default (fresh install and migration of an old install that never set it). It is sent
as `include_adult` (`false` or `true`) on every search, facet, breakdown and releases body,
and as `adult=1` on the saved-search poll when on (left out when off). Adult torrents are hidden, never deleted
(D52, D70); turning the switch on shows them again. Selecting category `porn` in the filter
sheet also shows them (server rule). For a server without `adult_filter` (pre-D52) the app
drops `category == porn` hits from the list itself, as the old app did. Nothing else: the app
has no detection of its own and does not make the server's stricter.

## 7. Upgrade from the old app (D81)

Same `applicationId` (`app.magnetar`), same release key (`android/keystore/keystore.properties`,
read by Gradle only), `versionCode` from the workspace version (≥ the installed one), so the
APK installs over the old app and Android keeps its data directory.

**Settings** (`SettingsStore`, schema `settings_version` 0 → 1, run on every start, idempotent):

| Old key (file `settings`) | New | Notes |
|---|---|---|
| `server_url` | `server_url` | trimmed, trailing `/` removed |
| `api_token` | `api_token` | |
| `root_npub` | `root_npub` | cleared if it holds an `nsec1…`/`ncryptsec1…` (never kept) |
| `trust_threshold` | `trust_threshold` | clamped 0–100; pushed to the server on the next Settings save |
| `show_adult` | `show_adult` | default false |
| `background_sync` | `notify_saved_searches` | the old switch ran crawl + saved-search poll; it now means the poll only |
| `cache_cap_mb`, `seed_relays` | removed | phone cache and phone crawler are gone |

File `ui`, key `asked_notifications` (one-time permission prompt) is kept as is.

**Leftovers of the old app**, removed at start (skeleton): the unique WorkManager works
`crawl-sync` and `crawl-sync-now` (their worker class no longer exists) and the notification
channel `downloads`. In the Saved stage: the old app's saved searches lived only in its SQLite
file `databases/magnetar.db` (table `saved_searches(name, query JSON, notify)`); they are read
once with the platform `android.database.sqlite` (no library) and created on the server
(`POST /api/v1/saved-searches`, skipping a name + query already there); only after every one
was created are `magnetar.db*` and the old `files/rqbit/` state deleted. Files the old app
downloaded (`Android/data/app.magnetar/files/Download/magnetar/`) are left alone.

## 8. Notifications (D84)

- Channel id `saved_searches` ("Saved search alerts", default importance) — the old app's id,
  so the user's per-channel choices (sound, silenced) survive. `downloads` is deleted.
- `SavedSearchPollWorker`: unique periodic work `saved-search-poll`, every **1 h**,
  constraint *network connected* (no charging / unmetered requirement: one small request per
  hour), enqueued while `notify_saved_searches` is on and the server lists
  `saved_search_poll`, cancelled otherwise.
- Watermark: the poll's `watermark`, stored in prefs file `notify` together with the server
  URL it belongs to; a different server starts with a first poll (no hits, server clock).
- One notification per saved search with new hits (InboxStyle, 5 titles, "+N more"); tap →
  `magnetar://torrent/{infohash}` of the newest hit. `POST_NOTIFICATIONS` is asked once
  (API 33+), Settings can ask again.

## 9. Theme and accessibility

- `MagnetarTheme` and `ChartColors` are the old files unchanged: Material You (dynamic) colours
  on Android 12+, the fixed indigo/red palette before; dark follows the system; charts keep the
  colour-blind-checked categorical order (D47) with separate light/dark steps.
- Every icon-only button has a `contentDescription`; chart bars and graph nodes have a text
  label and number next to them and a `semantics` description; status colours (seeded / dead
  / unknown) always come with a word. Touch targets ≥ 48 dp. Text uses `sp` and follows the
  system font scale; layouts are tested at 200 %. The search box has `ImeAction.Search`.

## 10. Build (D79)

Plain Gradle: `scripts/build-android.sh` = `./gradlew assembleDebug assembleRelease` (or
`GRADLE_TASKS`), creating the release key on first run as before. No cargo-ndk, no NDK, no
Rust. `android/app/build.gradle.kts` still reads `versionName` from `[workspace.package]
version` in `Cargo.toml`. `crates/ffi` and `crates/uniffi-bindgen` are removed from the
workspace (nothing else used them), and with them the `uniffi` and `rusqlite` workspace
dependencies. Release builds stay unminified for now (R8 with kotlinx.serialization needs its
keep rules checked first; tracked for the final stage).

Network security config: cleartext stays permitted (the server is chosen at runtime, often an
`http://` LAN or Tailscale address), and since OkHttp is governed by this file now, the
Settings warning for `http://` to a host that is neither loopback nor Tailscale (D39) is kept.

## 11. Stages

1. Server: API complete for a thin client (D76–D78, done).
2. **This document + the skeleton** (shell, theme, API client, settings store with
   migration, unit tests).
3. **Done (D85–D87)**: search, filter sheet, detail, title page, cache, and (brought forward
   from stage 4) saved searches, the poll worker and notifications, import of the old saved
   searches. Emulator UI test (§12).
4. **Done (D89–D91)**: Stats (charts, graphs, status) and the Settings screen (§13). The
   old stages 5 (stats and graphs) and 6 (settings) were merged into it.
5. Final integration check done (D94, §14). Not done: R8 minification (keep rules for
   kotlinx.serialization unchecked), so release builds stay unminified.

## 12. Search side as built (stage 3, D85–D87)

**Search** (`ui/search`). The text box feeds `SearchViewModel.onTextChange`; every change, filter,
sort, retry or settings change becomes one `SearchRequest` in a `StateFlow`, `debounce`d by
250 ms for keystrokes and 0 ms for the rest, then `collectLatest`: a newer request cancels the
one in flight (and with it the OkHttp call), so a late answer can never replace a newer one.
Each run: the cached first page, if any, shown at once with "cached · N min ago" (read before
`/health`, so offline it does not wait for the health call to fail, D88) → `/health` (cached per
server URL) → `POST /search` → the live page replaces it (scroll kept if the first hit
is the same) and is written to the cache. Offline / timeout / index building keep the cached
page with a banner; other errors clear it. The cache key is the query as asked (every filter,
before dropping those the server lacks) with the date preset instead of the clock, so "7 days" hits the same entry tomorrow. Rows: media line
"Title (year) · ★ 7.4 ›" (tap = title page), title, size · peers (tracker S/L, else "≈N peers"
from `best_peers`) · dead · age, attribute badges (the old ffi `badges`, now `ui/common/Labels.kt`,
checked against the Rust parser's output in `LabelsTest`), "matched in file: …", and the
source badge (trust star / DHT / both). "1,000+" when `total_is_lower_bound`.

**Filter sheet.** Opening it (and every draft change, 300 ms debounce) asks
`POST /facets {query: draft, fields: category, origin?, language…media_kind?, size: 40}`. Chip
order is fixed by the first counts seen (`ChipOrder`): a value that drops to 0 keeps its place,
disabled; new values append. The adult category chip is never disabled (with the switch off its
count is 0, and picking it is how the server shows those torrents). Back and swipe-down apply
like *Show N*. Release filters need `filters`, Source chips `origin`, counts `facet_set`;
`SearchFilters.supportedBy` drops what the server would ignore before anything is sent.

**Detail** (`ui/detail`): media card (2:3 poster, facts, genres, overview, IMDb, *All releases*;
asked once more after 4 s when the poster or overview is still missing, D42), attributes line,
*Open magnet* / *Copy magnet*, swarm card (*Check now* = `POST …/liveness`, then the history is
reloaded; seeders step chart drawn with Canvas from `liveness-history?days=90`, seeders =
max(tracker, DHT estimate), unanswered points skipped), DHT card or trust card, file tree,
trackers, tags, description, comments, other publishers. Cached for offline viewing.

**Title page** (`ui/title`): `POST /media/{id}/releases` with the Search tab's filters (no text,
sort seeders); "N search filters apply · Show all" clears them; in-page chips for season,
resolution and language group narrow the server's groups without a request.

**Saved** (`ui/saved`, `work/`): list (sorted by name), tap = run on the Search tab, delete,
notify switch (`PUT`, only with `saved_search_poll`; otherwise a "needs a newer server" banner),
create from the Search tab's star with the current text, filters, sort and adult switch. The
poll worker and the legacy import are as §7 and §8 describe.

**Emulator UI test** (`android/uitest/uitest.py`, D86): drives the debug build over adb with
`uiautomator dump` + `input` (Compose test tags are resource ids via `testTagsAsResourceId`,
also set inside the sheet, dialog and menu windows). Writes and deterministic checks go to the
bundled `fake_server.py`; `--production URL` adds read-only flows and the timing measurement.

## 13. Stats, graphs and settings as built (stage 4, D89–D91)

**Stats** (`ui/stats`): three tabs in the screen.

- *Charts*: the health card (`GET /stats/health`): seed checks as one status bar of the torrents
  checked at least once (seeded / dead / no answer, each with its number and share) and,
  **separately**, "Not checked yet: N (x % of all)", since never checked is not a result; then
  index state, relays answering, linked titles, names to parse, last crawl, and the DHT
  crawler's lines (mode, nodes, uptime, discovery rate, queue, metadata rate and success,
  stored with the adult share, blocked as a count only, queries, router ports, address class).
  Then the breakdowns (`POST /stats/breakdown`, top 10; years in a second request, top 50):
  donuts for category, resolution, movie/series/anime, Nostr/DHT; bars for the rest; a year
  histogram. **Tap** a bar, slice or year: it becomes a cross-filter (chips at the top; every
  other chart is counted again with it, its own chart is not). **Long press**: Search with that
  value plus the active cross-filters (a publisher category becomes the search text); *Search
  these* runs the chips. Requests are one flow with `collectLatest`, so a newer tap cancels the
  counts in flight. The adult switch goes into every breakdown body. Activity: one metric at a
  time (`GET /stats/timeseries`), days or hours, the DHT metrics (`dht_torrents_added`,
  `dht_hashes_seen`, `dht_fetch_ok`, `dht_fetch_fail`, `dht_blocked` as a count) when the server
  runs the crawler.
- *Graphs*: web of trust and relays cards; the title-releases graph opens from the title page's
  top bar and the swarm graph from the detail's swarm card (both need `graphs`). Graph screen
  (`ui/graph`): force layout off the main thread (D90), fitted to the view, pinch / drag / double
  tap to fit, tap a node for its card (*Open torrent*, *Open title*, *Copy npub*, *Copy URL*),
  *List* for the nodes as rows.
- *Status*: server (state, version, features it lacks), search index (state, searchable torrents,
  changes queued and their age, last update, size), seed checks (status bar, due now with the
  time to clear it at the last hour's pace, never checked, checked in 24 h, checks per hour,
  trackers backing off, last batch), relays and crawler, DHT crawler.

Health, breakdowns and graphs are cached (D83) and shown at once with "cached · N ago", kept
offline; the features of the last `/health` answer are cached too, so a server that is down when
the tab opens still gets the cached charts and graphs (D92). A server without `stats` gets a
one-line "older than the app" instead of the charts.

**Settings** (`ui/settings`), D91: Server (URL, token, *Test connection*), Trust (root key, *Get
my key from Amber*, threshold, *Save*), Content (*Show adult content*), Notifications
(*Saved-search alerts*, permission state, *Allow*), About (versions, TMDB / IMDb attribution).
Text fields are saved with *Save*; the switches apply at once.

**Emulator UI test** (`android/uitest`): stage 4 adds, on the fake server, the health card and
"not checked yet", tap = cross-filter (request body checked), long press = Search with the value
and the cross-filters, the activity chart, the web-of-trust graph (tap on the laid-out subject
node, then the node list), the Status tab, Settings *Test connection*, an nsec typed into the
root field (nothing of it stays), *Save* sending `PUT /settings/trust` with the hex root and the
threshold, and the adult switch reaching the charts. With `--production`: the health card,
the breakdowns and the web-of-trust graph, read-only (the Settings flow is never run there).

## 14. Final notes (integration check, D94)

- **Upgrade path proven on the emulator**: `dist/magnetar-v0.2.0.apk` installed, server URL,
  API token and trust root typed into its Settings, then `adb install -r` of the new release
  APK (same certificate, SHA-256 checked with `apksigner`; same `app.magnetar`, versionCode
  200): all three were in the new Settings screen and *Test connection* accepted the token; the
  first search answered with no setup. The trust threshold and the adult switch were left at
  their defaults in that run (the migration of both is covered by the unit tests in `SettingsStoreTest`).
- **Signing**: the build reads `android/keystore/` relative to the Gradle root. A worktree has
  no keystore (gitignored), and `scripts/build-android.sh` would then **generate a new key**,
  giving an APK that cannot update the installed app. Build release APKs from the main checkout,
  or point `android/keystore` at the main checkout's directory for the build only (what D94 did,
  link removed afterwards); never copy the key.
- **Sizes**: release APK 10.2 MB, debug 14.6 MB (old release 61.4 MB with the Rust library).
- **UI test**: `android/uitest/uitest.py` needs the debug build (it writes settings with
  `run-as`), so a debug APK replaces the release one on the emulator (different key: uninstall
  first). On a slow emulator `input text` right after a tap can lose its first character; rerun.
- **Adult content**: unchanged from D70; the app has no detection of its own and sends the
  Settings switch with every request.
- Open: R8 minification; a test on a real phone; posters (`/media/{id}/poster`) were not part of
  the integration smoke test (no TMDB token on the test server).
