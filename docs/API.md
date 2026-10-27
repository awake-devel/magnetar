# HTTP API for a thin client

What a client that talks only to the server's REST API (no Rust ffi, no phone-side crawl) uses,
endpoint by endpoint. The full table with request/response types is ARCHITECTURE.md §6; the
types are `magnetar_core::api` / `magnetar_core::model` (JSON field names = Rust field names,
enums in `snake_case`). Checked for the app rebuild in 2026-10 (D77).

## Basics

- Base URL: the server's address, e.g. `https://<host>`. JSON in and out.
- Auth: `Authorization: Bearer <MAGNETAR_API_TOKEN>` on everything except `GET /health`, the
  poster images included (an image loader needs the header, e.g. an OkHttp interceptor).
- Errors: `{"error": "<code>", "message": "…"}` with 4xx/5xx. Codes: `unauthorized`, `invalid`,
  `invalid_body`, `not_found`, `search_index_unavailable` (503 while the index builds), `downloads_unavailable` (503),
  `upstream` (502), `timeout`, `store`, `internal`.
- Feature detection: `GET /health` → `{"status","version","db","features":[…]}`. A client
  hides what the server does not list. Since this stage: `saved_search_poll` (D77) and
  `browse_checked_dht` (D76), next to `filters`, `facet_set`, `media_releases`,
  `liveness_history`, `stats`, `graphs`, `adult_filter`, `origin`, and `dht` (crawler on).
- Unknown query parameters are ignored; unknown JSON fields are ignored; every newer field has
  a default, so an older server's answers still parse.

## What each screen needs

| Need | Endpoint | Notes |
|---|---|---|
| Search / browse | `GET /api/v1/search?q=…` or `POST /api/v1/search` (`SearchQuery`) | `SearchPage {hits, total, source, total_is_lower_bound}`. Query string: `q, cat, min_size, max_size, since, until, min_seeders, hide_dead, min_trust, sort (relevance/date/size/seeders/rating), desc, limit (≤ 200), offset, lang, sub, res, source, codec, hdr, audio, genre, kind, min_rating, min_year, max_year, origin (nostr,dht), adult=1`. `min_trust` omitted = the server's threshold. |
| Adult switch (off by default) | `adult=1` / `"include_adult": true` on every search, facet, breakdown, releases and saved-search poll | Adult torrents are hidden, never deleted (D52, D70). Selecting category `porn` also shows them. |
| Empty-query browse | same | D76: no text and no source filter → DHT-only torrents only once a seed check found peers. Text searches and `origin=dht` list every DHT result. |
| Result row | `TorrentSummary` | `title, category, total_size, file_count, created_at, seeders, leechers, dead, best_peers` (max of tracker seeders and DHT peer estimate; show this when `seeders` is null), `trust, publisher_count, origin (nostr/dht/both), media_title, media_year, media_kind, media_id, media_rating, attrs` (resolution, languages, …), `matched_file`. |
| Filter sheet counts | `POST /api/v1/facets` (`{query, fields, size}`) or `GET /api/v1/facets?field=…` | Disjunctive counts (docs/FILTERS.md); 503 while the index builds. |
| Detail | `GET /api/v1/torrents/{infohash}` | `TorrentDetail`: summary, `magnet` (ready to hand to a torrent app), files, description, events, comments, trackers, `trust` (score + factors), `media` (`MediaInfo` incl. `poster`), `dht` (`DhtInfo` for DHT rows). 404 unknown. |
| Seed check now | `POST /api/v1/torrents/{infohash}/liveness` | `Liveness {seeders, leechers, dht_peers_est, source, checked_at, expires_at, …}`. Works for DHT-only torrents since D77 (was 404). |
| Seed history chart | `GET /api/v1/torrents/{infohash}/liveness-history?days=N` | ≤ 90 days, oldest first. |
| Title page | `GET /api/v1/media/{id}/releases` (+ search query string) or `POST` (`SearchQuery`) | `MediaReleases {media, total, truncated, groups}`; `GET /api/v1/media/{id}/torrents` = flat `SearchPage`. |
| Posters | `GET /api/v1/media/{id}/poster` | Image bytes, `ETag` + `If-None-Match` → 304, `Cache-Control: private, max-age=604800`; 404 when there is no poster or TMDB is not configured. URL: `MediaInfo.poster` (server-relative), or built from a hit's `media_id`. No backdrops or stills. |
| Stats tab | `GET\|POST /api/v1/stats/breakdown`, `GET /api/v1/stats/timeseries`, `GET /api/v1/stats/health`, `GET /api/v1/stats` | docs/STATS.md. Breakdowns count the whole catalogue (D76 does not apply). |
| Graphs | `GET /api/v1/graphs/wot`, `/relays`, `/media/{id}`, `/swarm/{infohash}` | `Graph {kind, nodes, edges, truncated, note}`. |
| Trust settings | `GET` / `PUT /api/v1/settings/trust` (`{"root": npub-or-hex or null, "threshold": 0–100}`) | |
| Block / trust a publisher | `PUT /api/v1/overrides/{pubkey}` (`{"action": "block"\|"trust"\|null}`) | 204. There is no list endpoint: a client that shows "blocked publishers" keeps its own list. |
| Publisher trust | `GET /api/v1/trust/{pubkey}` | `TrustScore`. |
| Relays | `GET` / `PUT /api/v1/relays` (`{"url","enabled"}`) | The server's crawl relays. |
| Saved searches | `GET /api/v1/saved-searches`, `POST` (create, `id` 0), `GET\|PUT\|DELETE /api/v1/saved-searches/{id}` | Stored on the server (`SavedSearch {id, name, query, notify, last_checked, last_seen_created_at}`). `POST {"name","query","notify"}` is enough (every field defaults). `PUT` takes name, query and notify and keeps the server notifier's watermarks (a changed query resets them). An edit made while the server notifier runs is kept: the notifier writes only its watermark columns. |
| Notifications | `GET /api/v1/saved-searches/poll?since=<watermark>[&adult=1][&all=1][&id=1,2]` | See below. |
| Server downloads | `GET\|POST /api/v1/downloads`, `POST /api/v1/downloads/{id}/{pause\|resume}`, `DELETE /api/v1/downloads/{id}` | Only when the server runs with downloads; 503 `downloads_unavailable` otherwise. |

## Saved-search notifications (D77)

`GET /api/v1/saved-searches/poll` runs the saved searches with `notify` (all of them with
`all=1`; `id=` narrows to a comma list) and returns what is new since the client's watermark:

```json
{"watermark": 1790000050,
 "results": [{"search": {"id": 1, "name": "Ubuntu", "query": {…}, "notify": true, …},
              "hits": [TorrentSummary, …], "truncated": false}]}
```

- Stateless on the server: nothing is written. The client stores `watermark` and sends it back
  as `since`. So two phones, a phone and the server's own ntfy notifier (D25) never take each
  other's new hits.
- First poll (no `since`): no hits, `watermark` = the server's clock.
- "New" = `created_at > since` (the publication time, as the ntfy notifier and the old phone
  poller): hits newest first, at most 200 per search (`truncated` when there were more); the
  next watermark is the newest `created_at` returned, or `since` when nothing matched. An event
  that reaches the server late with an older `created_at` than the watermark is not reported
  (Nostr events are accepted up to 15 min in the future, so a watermark cannot run ahead by
  more than that). The same holds for a DHT torrent stored long after it was first sighted
  (its `created_at` is the first sighting), and for a saved search with no text, where D76
  lists a DHT-only torrent only after a seed check: such a torrent can predate the watermark by
  the time it becomes visible. A per-arrival watermark would need an arrival time in the
  search index; not done.
- `adult=1` adds the client's adult switch to every query; otherwise each saved query's own
  `include_adult` applies.
- A periodic poll from Android WorkManager (e.g. 15 min) is enough; each poll is one search per
  saved search.

## Crawl workers

`/api/v1/worker/*` is for `magnetar-server worker` only (worker tokens, docs/WORKERS.md): the
app's API token gets 401 there. `GET /api/v1/stats` gains `workers` (one `WorkerStats` per
configured worker: connected, version, leases held, seed results and metadata per minute, DHT
state; empty without workers), and `/health` lists the feature `workers` when tokens are
configured. Clients may ignore both.

## Not in the API (phone-local by design)

- Pinned / hidden torrents: client state.
- The phone's own Nostr crawl, sync and SQLite cache (ffi only): a thin client has no offline
  search; it can cache pages itself.
- Downloads on the phone: a thin client hands `TorrentDetail.magnet` to a torrent app (or uses
  the server downloads above).
