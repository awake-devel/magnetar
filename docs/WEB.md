# The web client (D132–D135)

A browser frontend for the server, served by the server itself at its root
(`https://<host>:8443/` through `tailscale serve`). It is the Android app's feature set
(docs/APP.md) as a web page: Search with every filter and live facet counts, results, torrent
detail, title pages, saved searches, Stats, Settings. It works at phone width and installs as a
PWA. Nothing is built or bundled: plain ES modules in `crates/server/web/`, embedded in the
binary at build time (D132). **UNVERIFIED**: Safari / iOS, PWA install on Windows and macOS,
real phones.

## 1. What it is, and what differs from the app

| | Android app | Web client |
|---|---|---|
| Server | URL field in Settings | the page's own origin (no URL field) |
| Sign-in | API token stored on the phone | the token is sent once to `POST /api/v1/web/login`; the browser keeps an HttpOnly session cookie (§2) |
| Trust root | paste, or **Amber** (NIP-55) | paste an npub, nprofile or hex key; Amber is Android-only |
| Saved-search alerts | hourly local poll + notifications | the server's ntfy notifier (D25); the page uses **no browser push** and says so in Settings |
| Saved searches | run, delete, notify | the same, plus an Edit dialog (name, notify, replace the query) |
| Stats long press | long press | an explicit "Search for X" action (a 500 ms press also works on touch) |
| Graph and map gestures | pinch, drag | wheel or pinch zoom, drag; touch still works |

## 2. Security model

Every string from the API is untrusted: torrent titles, file names, track names, descriptions,
comments and publisher names are written by peers and strangers.

- **No markup, ever.** `js/dom.js` is the only way the UI builds DOM: `h()` (createElement,
  properties from an allow-list; `innerHTML`, `style`, `srcdoc`, inline `on*` and raw `href` /
  `src` strings throw), `svg()` (createElementNS, geometry attributes only, numeric path data),
  `untrusted(text)` (a `<span class="u" dir="auto">` with `unicode-bidi: isolate`, so a
  right-to-left override in a title cannot flip the layout around it; the app's own text next
  to it, such as " (2024) · ★ 8.5", sits in its own text node). Descriptions go through
  `stripHtml` (the app's Format.kt) and are shown as text with `white-space: pre-wrap`.
- **URLs only from validated parts**: `link(route, params)` (infohash `^[0-9a-f]{40}$`, media id
  a safe integer, graph path `^(wot|relays|media/\d+|swarm/<ih>)$`), `magnetLink(detail)` (the
  API's own `magnet` field, only when it starts with `magnet:?` and names this infohash; a magnet
  is never built on the client), `externalUrl()` (IMDb `tt\d{7,10}`, db-ip.com, themoviedb.org;
  `rel="noopener noreferrer"`, new tab), `posterImg()` (same-origin `/api/v1/media/<id>/poster`
  only, `referrerPolicy = no-referrer`, a placeholder on error).
- **The sink lint** (`test/sinks.test.js`) fails the build if any web JS file contains
  `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `DOMParser`,
  `createContextualFragment`, `eval(`, `new Function`, string timers, `setAttribute('on…')`,
  `setAttribute('style')`, `alert(`, `confirm(`, `prompt(`, `javascript:`, `.srcdoc`,
  `.cssText` or a non-literal `import()`. Nothing is allow-listed. `test/dom.test.js` checks
  `dom.js` itself against a fake document. Dialogs are in-page `<dialog>`s; there is no
  native alert, confirm or prompt anywhere.
- **CSP with Trusted Types** (server side, D134): `default-src 'self'`, no inline script or
  style, `require-trusted-types-for 'script'`. The page creates exactly **one** Trusted Types
  policy, `magnetar` (`js/tt.js`), with only `createScriptURL`, which accepts `/sw.js` and
  `/_app/<V>/js/*-worker.js` and nothing else. There is no `createHTML`, so every markup sink
  still throws. Why a policy at all: under `require-trusted-types-for 'script'` the URL given
  to `navigator.serviceWorker.register()` and to `new Worker()` must be a `TrustedScriptURL`,
  and `trusted-types 'none'` allows no policy, so the offline shell and the graph layout worker
  could not start. The server's CSP therefore says `trusted-types magnetar` (D134). The page
  reads its own CSP header first and skips the service worker, without a CSP violation, when a
  server does not allow the policy.
- **Session cookie, not a token in the page.** A token in `localStorage` would be readable by
  any script that ever ran in the page, and the API token is the master key (every endpoint,
  shared with the phone and the workers' configs, revoked only by redeploying). An HttpOnly
  `SameSite=Strict` cookie cannot be read by script; the session expires after 30 days, can be
  signed out (this browser or every browser) and dies when the API token changes. The page keeps
  only the CSRF token, in memory, and gets it back from `GET /api/v1/web/session` after a
  reload. It sends `X-CSRF-Token` on every POST, PUT and DELETE; a 403 `forbidden` makes it ask
  the session again and retry once. A 401 clears the CSRF token, the page cache and the
  "signed in" flag and goes to `/login?next=…&ended=1`, where the page says "Your session
  ended: sign in again" (the flag stays in the URL until the next sign-in). A full page load
  whose session check answers 401 while this browser's "signed in" flag is set goes there too
  (`session.boot()` answers `ended`). The cookie is always
  `__Host-magnetar_session` with `Secure`, also on `http://127.0.0.1` (browsers treat loopback
  as a secure context), so no proxy `Host` can ever yield a non-Secure session cookie. `next`
  is followed only when it is a same-origin path (`/x`, never `//x`). On plain `http://` to a
  host that is neither loopback nor `*.ts.net` the page shows the D39 warning, worded for the
  session cookie.
- **No secret in browser storage.** localStorage holds `magnetar.showAdult`,
  `magnetar.features` (the last `/health` features), `magnetar.lastSearch` (a query string; the search state of a page loaded on any route
  starts from it, so Saved, a title page and the Search tab see the last search),
  `magnetar.signedIn` ("1", so an offline start shows the cache instead of the sign-in page),
  and the Stats tab and chart choices. All reads and writes are wrapped: the page works when
  storage is blocked.
- `document.title` is "<view> · Magnetar"; the view part is fixed text, never a torrent title,
  so a hostile name never lands in the tab or the history.

## 3. Module map (`crates/server/web/`)

```
index.html               the shell page (no inline script or style)
sw.js                    service worker: app shell only (§5)
manifest.webmanifest     PWA manifest; icons/ (icon.svg, 192/512 PNG, maskable 512 PNG)
css/app.css              tokens on :root, light and dark, the three layouts
js/main.js               boot: shell, session, /health, router, service worker
js/router.js routes.js   history routing, route table (views load on first use)
js/dom.js icons.js tt.js DOM building, icons, the Trusted Types policy
js/api.js                same-origin fetch, timeouts per class, ApiError, CSRF header
js/session.js            sign-in, session, sign-out, 401 handling, server trust settings
js/health.js             /health features (the gate), cached, refreshed every 5 min
js/pagecache.js          IndexedDB page cache (§5)
js/prefs.js store.js     localStorage conveniences; app state (signed in, trust, update)
js/format.js numfmt.js labels.js search-model.js nip19.js settings-checks.js
                         ports of the app's pure Kotlin (Format, NumberFormat, Labels,
                         SearchModel, Nip19, SettingsChecks), unit-tested with the app's values
js/probe-format.js probe-correction.js saved-model.js chart-colors.js charts.js stats-model.js
   sources-model.js probes-model.js crawlmap-model.js worldmap.js graph-layout.js
                         ports of ProbeFormat, ProbeCorrection, SavedScreen's summary,
                         ChartColors, Charts/SeederChart geometry, StatsModel, SourcesModel,
                         ProbesModel, CrawlMapModel, WorldMap's parser and ForceLayout (§8–§11)
js/graph-worker.js       the graph layout's module Worker
js/types.js types-stats.js  JSDoc mirrors of the API JSON
css/screens2.css         detail, saved, stats, graph and crawl-map styles
assets/world_110m.bin    the crawl map's world outline (byte copy of the app's asset)
js/ui/                   shell (rail / tabs, page header), dialog, snackbar, chips, banner, slider
js/views/                login, search (+ search-controller, filters, results, result-row,
                         save-dialog), title, settings, not-found; detail (+ media-card,
                         swarm-card, seeder-chart, dht-card, trust-card, probe-card,
                         probe-editor, file-tree), saved (+ saved-edit-dialog), stats (+
                         stats-charts, stats-graphs, stats-status, sources-card, probes-card),
                         graph, crawl-map (§8 onwards)
test/                    deno tests (pure modules, the DOM helper against a fake document, the
                         API client with a fake fetch, the sink lint)
```

Every view exports `mount(el, {params, query, ctx}) → {unmount(), update?(params, query)}`.
`ctx` is `{api, store, health, prefs, nav, back, snackbar, dialog, layout, session, cache}`
(`layout` = `phone` | `medium` | `wide` from `matchMedia`; `pane: true` when a view is mounted
in the search workspace's detail pane). `ui/shell.js` exports `pageHeader(ctx, {title, back,
actions})` and `iconButton()`; `views/search.js` exports `runSearch(ctx, query)` (run a
`SearchQuery` on the Search tab: saved searches, "Search for X") and `searchController(ctx)`.

## 4. Routing and URL parameters

| Path | View |
|---|---|
| `/` (`/search` redirects) | Search |
| `/saved` | Saved searches |
| `/stats`, `/stats/graphs`, `/stats/status` | Stats tabs (one view) |
| `/settings`, `/login` | Settings, sign-in |
| `/torrent/<40 hex>` | Detail: full page on phones, the right pane on medium and wide |
| `/title/<id>` | Title page |
| `/graph?path=` | A network graph |
| `/crawl-map?layer=&window=&metric=` | Crawl map |
| anything else | "Not found" with a link to Search (a direct page load of an unknown path gets the app with status 404; `/api/…`, `/_app/…`, `/relay` and `/health` keep their JSON) |

Links carry `data-route` and are handled without a page load (a modifier key or middle click
opens a new tab as usual); Back and Forward re-mount the view and restore the scroll.

The Search state lives in the query string (replaced, not pushed, so Back is not filled with
keystrokes), with the API's own GET names: `q, cat, lang, sub, res, source, codec, hdr, audio,
genre, kind, origin, probe_lang, probe_sub, probe_res, probe_codec, probe_hdr, probe_audio,
probe_audio_kind, probe_sub_kind, probe_sub_format` (comma lists), `min_seeders, hide_dead=1,
min_trust, min_rating, min_year, max_year, sort, desc=0`, plus the web-only `date=day|week|
month|year` and `min_size_mb` / `max_size_mb` (MiB; the API's byte `min_size` / `max_size` are
read too). Unknown parameters are ignored and malformed ones dropped, never an error. A reload
restores the search; the Search tab returns to the current search.

## 5. Layout, caching and offline

- **Phone** (< 768 px): one column, a top bar (Magnetar, Save search, Sort, Filters with a
  badge), a bottom tab bar (Search, Saved, Stats, Settings; 48 px targets; safe-area inset). The
  filter sheet is a bottom `<dialog>` holding a draft; Esc, the scrim, × and "Show N" apply it.
- **Medium** (768–1199 px): left rail, results and the detail pane; filters in a side drawer
  (same draft rules).
- **Wide** (≥ 1200 px): rail, a filters sidebar (each change applies after 300 ms, live
  counts, "Clear all"), results, the detail pane. A detail opened directly shows the last
  search of this browser beside it.
- No horizontal page scroll from 320 px up: untrusted strings wrap anywhere.

**Page cache** (`js/pagecache.js`, IndexedDB `magnetar-pages`): the app's PageCache. Key = SHA-256
of the endpoint and the canonical request (sorted keys, the date preset instead of the clock, the
adult switch included). 300 entries, least recently used out. A cached answer shows at once
("cached · 5 min ago") and is replaced by the live one; on a network error, timeout or index
build it stays with "Offline: <reason>. Showing the cached page."; any other error clears it.
Cleared at sign-out and on any 401. Search page 1, title pages, details and the Stats answers are
cached. Opening the database gives up after 2 s (an open queued behind a blocked delete or
upgrade, after "Clear site data" with another tab open, gets no event at all, and the boot's
sign-out path would otherwise wait forever on a blank page), and a connection closes itself on
`versionchange` so it never blocks another tab.

**Service worker** (`sw.js`, scope `/`): caches the app shell (`/` and every `/_app/<V>/` asset
the server lists) under `magnetar-shell-<V>`, drops older versions on activate. `/_app/<V>/*`
is cache-first; navigations to an SPA route are network-first with a 4 s timeout, then the
cached shell. `/api/*`, `/health`, `/sw.js` and the manifest are never intercepted, and no API
answer is ever stored by it. A new version installs in the background and waits: the page shows
"Updated: reload to use the new version", and its Reload asks the waiting worker to take over
(`SKIP_WAITING`), then reloads on `controllerchange`, so an open tab never mixes versions. The
page looks for a new version when it comes back to the foreground. It registers only in a secure context (https, or 127.0.0.1) and only when the CSP
allows the `magnetar` Trusted Types policy (§2).

## 6. Feature gating

`/health` features gate every control, as in the app (docs/APP.md §4): `filters` (release filters,
rating sort), `origin` (Source chips, source badge), `facet_set` (counts; without it the chips are
the built-in lists), `probe_filters` and `probe_kinds` (probe chips, kind chips), `media_releases`
(title pages and media-line links), `liveness_history`, `graphs`, `saved_search_poll`, `stats`,
`crawl_sources`, `crawl_map`, `workers`, `probe_corrections`, `adult_filter` (without it the page
drops `porn` hits itself while the switch is off), `dht`. `supportedBy` drops a filter the server
would ignore before anything is sent. On a first sign-in nothing is cached yet: the search waits for
`/health`, and the filter panel asks for its counts again when the features arrive
(`onFeaturesChange`). The adult switch goes out as `include_adult` on every
search, facet, breakdown and releases body.

## 7. Checks

From `crates/server/web/` (deno 2.x, offline; `scripts/test.sh` runs the same):

```sh
DENO_NO_UPDATE_CHECK=1 deno lint
DENO_NO_UPDATE_CHECK=1 deno task check   # deno check js/ test/ sw.js (checkJs, strict)
DENO_NO_UPDATE_CHECK=1 deno task test    # deno test --allow-read=.,<the app's world map asset> test/
```

Tests import only relative modules and `node:assert/strict`; the only permissions are reading the
web directory (the sink lint, the world map) and the app's `world_110m.bin` (the test checks the
web copy is the same bytes). The browser check against a seeded local server is
`scripts/webcheck.sh` (§12).

## 8. Detail and track corrections

`/torrent/:ih` (`views/detail.js`; DetailScreen.kt, DetailViewModel.kt). Full page with Back on
phones; in the search workspace's right pane on medium and wide layouts (`ctx.pane`, no Back).

- **Loading**: the cached detail (page cache key `detail/<ih>`) shows at once, then
  `GET /api/v1/torrents/{ih}` replaces it. When the media has no poster or overview yet, the
  page asks once more after 4 s (D42). Offline, timeout or index building: the cached detail
  stays under "Offline: … Showing the cached page."; any other error clears it. 404 reads "Not on
  this server".
- **Head**: category, title (untrusted, bidi-isolated), size · files · peers · dead · date, the
  category path, the release attributes (`attrDetails`). "Open magnet" is an `<a href>` to the
  API's own `magnet` (only when it is a `magnet:?` URL for this infohash, `dom.magnetLink`), with
  "Nothing happened? Copy the magnet into your torrent app."; "Copy magnet" uses the clipboard API
  (snackbar "Magnet copied", or "No magnet in the answer"). A magnet is never built here.
- **Media card** (`media-card.js`): poster through `posterImg` (same origin only, a placeholder
  without one), "Movie · ★ 7.4 (812k) · 155 min", genres, "S01E02 · title", the overview (click
  to show it all), IMDb (fixed base, `noopener noreferrer`), "All releases" (`media_releases`).
- **Swarm** (`swarm-card.js`, `seeder-chart.js`): "Swarm seeded|dead|unknown: S seeders · L
  leechers" (the status always in words), DHT estimate, "checked … ago · via … · trackers a ok / b
  failed", or "Not checked yet". "Check now" → "Checking…" (`POST …/liveness`, 45 s class); a 429
  reads "Check failed: too many seed checks right now; try again later"; the history reloads after.
  The chart (`liveness_history`, 90 days) is an SVG step line, seeders = max(tracker, DHT
  estimate), unanswered points skipped, with "max N", a legend, the first date and "now", and a
  caption that says the numbers; "No seed checks recorded yet" without points. "Swarm graph"
  (`graphs`).
- **DHT card** for torrents found on the DHT, **Trust card** otherwise (`dht-card.js`,
  `trust-card.js`): score with its level in words, "Show / Hide trust factors", factors, computed
  age, root, Publisher npub with Copy, "N publishers announced this torrent", proof of work, "Also
  published by".
- **Files** (`file-tree.js`): folders first, sizes summed, open when ≤ 30 files, 300 rows then
  "… N more". **Trackers**, **Tags** (external ids copy on click), **Description** (`stripHtml`,
  shown as text with `pre-wrap`, never a link), **Comments** ("npub · age"), **Infohash** with Copy.

**Detected card** (`probe-card.js`, `probe-format.js`, D121, D126). Without `probe_corrections`
(an older server) the D126 lines. With it, per probed video file: container and video lines, then
one row per audio track and per subtitle track, numbered as VLC numbers them ("Track 2 · French ·
AC-3 2.0 · commentary"), the name on a second line in quotes after `cleanTrackName` (bidi and
control characters removed, 64 UTF-8 bytes), the markers "✓ matches VLC", "corrected", "not in the
file" (struck through, and the word says it), "added by you", the review warnings, and "Show /
Hide detected" on a corrected row. More than 10 subtitle rows: "Show all N subtitle tracks" (stays
open after a save). Per file: "Add a track the probe missed" and "All tracks match VLC" ("Marked N
tracks as checked"). Then "Corrections that no longer match the probe" with Remove, the flags'
warnings, console lines, notes, and the footer "Read from the first blocks of each file, not from
its name." with status, hash check and probe age. Every line read from a file is untrusted text.

**Editor** (`probe-editor.js`, a `<dialog>`, full screen on phones): title "Audio track N",
"Subtitle track N", "Added … track" or "Add a track the probe missed"; the VLC hint; "Detected:
…" and "Name in the file: “…”"; Audio / Subtitle when adding; Language with "Change" (an inline
search: "Unknown / none" first, "French (fre)", a /T code finds its /B language); kind chips in
D126 order; "Track name (as VLC shows it)" (cleaned while typing); "This track doesn't exist"
(turns the other fields off). Buttons: "Undo correction" (a correction exists), Cancel, and Save /
"Matches VLC" / "Still correct" (`primaryAction`). Sending (`POST …/probe/corrections {device:
"web", items}` or `DELETE …/{id}`) shows a progress bar, turns every control off, and Esc or the
backdrop do nothing. The answer replaces the probe and the cached detail ("Correction saved",
"Correction removed"); a failure keeps the dialog and the draft ("Offline: the correction was not
saved" / "Couldn't save the correction: …"). A detail fetch that started before a correction
landed keeps the corrected probe. The request items are `probe-correction.js` `itemOf`, tested
with the app's ProbeCorrectionTest values (fields left out of the JSON, as the app's
`explicitNulls = false`).

## 9. Saved searches

`/saved` (`views/saved.js`, `saved-model.js`; SavedScreen.kt). The list by name (case-insensitive),
each with its name (untrusted), the summary line (`describe`: "\"dune\" · 2160p (4K)", "any text ·
files: French, SDH", …) and "checked N ago". A click runs it (`runSearch`: the Search tab with its
text, filters and sort, URL updated). The Notify switch sends `PUT` at once and reverts with "Not
changed: …" on a failure; it is off without `saved_search_poll` ("Notifications and editing need a
newer server (saved_search_poll)."). Edit (a web addition, `saved-edit-dialog.js`): Name, Notify,
"Replace its query with the current search" (with that search's summary). Delete asks in an
in-page dialog ("Delete “name”?"), then "Deleted “name”" or "Not deleted: …". Without an ntfy topic
on the server: "The server has no ntfy topic: Notify sends nothing yet." Alerts are the server's
ntfy notifier's (every 15 min): the page uses no browser push. The list is in the page cache
(`saved`) and shows offline with its age.

## 10. Stats

`/stats`, `/stats/graphs`, `/stats/status` share one mounted view (`views/stats.js`); the last tab
is remembered (`magnetar.stats.tab`), so the Stats item of the rail opens it again. Each answer is
in the page cache (`stats/health`, `stats/breakdown` per request, `stats/timeseries` per metric and
bucket, `stats/sources`, `stats/fleet`) and shown offline with its age.

- **Charts** (`stats-charts.js`, `charts.js`, `stats-model.js`): the Health card (status bar
  seeded / dead / no answer, each with number and share, then "Not checked yet: N (x % of all)",
  index, relays, linked titles, names to parse, last crawl, DHT lines with `dht`); the breakdowns
  (`POST /stats/breakdown`, top 10, years in a second request, top 50): donuts for categories,
  resolutions, movies/series/anime and Nostr / DHT with their values listed beside them, bars for
  the rest, the year histogram with "peak …", "Other: N in M more values · Unknown: N". A click on a
  bar, slice or year adds a cross-filter chip and recounts every other chart (a newer click aborts
  the older request); "Search these" and "Clear". The secondary "Search for X" is a search button
  on each value (shown on hover or focus) and a 500 ms long press on touch; a publisher category
  becomes the search text. Activity: metric chips (DHT ones with `dht`, "Probe backfill DHT rows"
  when the server sends probe backfill counts), Days / Hours (remembered), an SVG line (gauges) or
  columns (counters), the axis ends naming the day in Hours and on the 48 h columns ("Mon 17:00"); click or arrow keys read a value, the wheel zooms, a drag pans, double-click
  resets. "Where discoveries come from" (`sources-card.js`, `crawl_sources`): DHT routes with seen /
  stored bars and conversion, the 48 h stacked columns (click a column for its hour), Nostr relays
  by source, top relays, machines, and the Crawl map button (`crawl_map`). Relay hosts and machine
  names that are IP literals are replaced by "(IP address)" before anything shows or caches them
  (`sources-model.redactIpHost`, as the app).
- **Graphs** (`stats-graphs.js`): "Web of trust" and "Relays and publishers", and the note on title
  and swarm graphs. Without `graphs`: "This server is older than the app and has no graphs."
- **Status** (`stats-status.js`, `probes-card.js`): Server (state, version, "Needs a newer server
  for …"), Search index, Seed checks, Relays and crawler, DHT crawler ("Turned off on this server"
  without one), Header probes (`workers`: backfill bar and percent, never "100 %" while something
  is left, the queue lines and closed outcomes, one block per worker with connected / not
  connected in words and its badges, then "Your corrections").

Every chart is SVG with `role="img"` and a label that says the numbers, plus its values as text
(rows, legends or a "Values" list). Status colours always come with a word.

## 11. Graph and crawl map

**Graph** `/graph?path=` (`views/graph.js`, `graph-layout.js`, `graph-worker.js`). The path must
be `wot`, `relays`, `media/<id>` or `swarm/<infohash>`; anything else shows "The server has no such
graph…" without a request. The force layout (a port of ForceLayout.kt, seeded the same way) runs
in a module Worker started through the page's Trusted Types policy (`tt.scriptUrl`, the policy
allows exactly `/_app/<V>/js/*-worker.js`); where the policy is not allowed it runs on the page in
slices. Canvas drawing; wheel or pinch zoom, drag to pan, double-click (or `0`) to fit, arrow keys
pan; a click selects a node: its card (label and details untrusted, kind, trust, links) with Open
torrent, Open title, Copy npub or Copy URL. "List" shows every node as a row. "N nodes, M links",
"Only the N most important nodes are shown.", "Nothing to show yet.". Canvas labels have bidi and
control characters removed.

**Crawl map** `/crawl-map?layer=&window=&metric=` (`views/crawl-map.js`, `crawlmap-model.js`,
`worldmap.js`). The world outline is a byte copy of the app's `world_110m.bin`
(`assets/world_110m.bin`, in the service worker's shell cache; a test checks the bytes). DHT
discoveries / Nostr relays, Day / All, metric chips from the answer (another layer resets the
metric); a newer choice aborts the older request; the URL follows. Canvas choropleth on a log
scale from mapLow to mapHigh, mapEmpty without data, legend "Log scale from a to b". A click on a
country shows "Name (CC)" (`Intl.DisplayNames`) with every metric and its share; again clears it.
Wheel or pinch zoom up to 8×, drag when zoomed, double-click resets; unzoomed, a touch drag and a
downward wheel still scroll the page. "Top countries" and "Top networks (AS)" (15 rows, "Show all
N" / "Show fewer"), "Location unknown: N (x %)". Banners by `reason` (CrawlMapModel.kt texts) and
"This server has no crawl map (it needs a newer version)." on a 404. A fixed bar at the bottom:
"IP Geolocation by DB-IP" (link) "(CC BY 4.0, database of <month>) · Map: Natural Earth (public
domain)".

## 12. Checking it

- `deno task test` (§7) covers the ports with the app's own test values: ProbeFormatTest,
  ProbeCorrectionTest, StatsFormatTest, ProbeStatsTest, CrawlMapTest, GraphLayoutTest,
  FileTreeTest (seed history), ScreensViewModelTest (`describe`); the "Detected" card against a
  fake document (no edit action without `probe_corrections`, track names as isolated text); the
  world map bytes against the app's asset.
- The browser check runs against a seeded local server, driven with Claude in Chrome on
  `http://127.0.0.1:18787` only (the parity checklist of the D132 spec). With the Postgres test
  container up on 127.0.0.1:54329:

  ```sh
  S=/some/scratch/dir                     # token, worker tokens, fixtures.json, state, pid
  scripts/webcheck.sh "$S" seed           # recreate magnetar_webcheck, write $S/fixtures.json
  scripts/webcheck.sh "$S" serve          # build and run on 127.0.0.1:18787 (foreground)
  scripts/webcheck.sh "$S" audit          # every socket of the server must be loopback
  scripts/webcheck.sh "$S" stop           # SIGINT to the server started by serve
  WEBCHECK_NTFY=1 scripts/webcheck.sh "$S" serve   # the ntfy-on variant (loopback target)
  ```

  Sign in with the contents of `$S/token` (random, mode 600, never printed). The server runs
  with every outbound activity off: no Nostr crawl or NIP-66, DHT, DHT probe, DHT seed checks,
  downloads, IMDb or anime datasets, GeoIP database, TMDB, and
  `MAGNETAR_LIVENESS_FALLBACK_TRACKERS=false` (D136); the seeded trackers are
  `127.0.0.1:9`, so "Check now" stays on loopback. Fixtures W1–W12 are described in
  `crates/server/examples/webcheck_seed.rs` and their infohashes are in `fixtures.json`.
- **UNVERIFIED**: Safari and iOS, the PWA install on Windows and macOS, real phones, 200 % zoom.
