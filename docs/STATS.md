# Stats, charts and graphs (D47)

The app's **Stats** tab shows what the server's index holds, how it grows, how healthy it is,
and four interactive network graphs. Everything is computed on the server and sent as small
aggregates; the phone draws them. Offline, the tab shows counts of the phone's own cache.

## Where things are in the app

The bottom bar keeps five tabs: Search, Saved, Downloads, **Stats**, Settings. Stats replaced
the Status tab (adding a sixth tab would crowd the bar on small phones), and the old Status
screen is its third section:

| Section | What it shows |
|---|---|
| Charts | health card; breakdowns of audio languages, resolutions, categories, sources, video codecs, HDR, audio formats, genres, movies/series/anime, publisher categories, years; activity over time |
| Graphs | web of trust, relays and publishers (a title's releases open from its title page, a torrent's swarm from its detail page) |
| Status | local cache, server, seed checks, relays (unchanged) |

**Cross-filtering.** Tap a bar, slice or year: it becomes a filter (chip row at the top) and
every other chart is counted again with it. A chart is never filtered by its own selection,
so "French" stays one bar among the other languages while the resolution chart shows French
releases only. **Long-press** a value to open Search with it (plus the active filters);
"Search these" runs a search with all active filters. Values that are not search filters
(publisher categories) open a text search.

**Activity.** Pick a metric and hours or days; pinch to zoom, press for a value. Counters are
columns (one per hour or day, UTC buckets), gauges a line.

**Detail page.** The swarm card shows the seeders/leechers history (step lines, last 90 days,
D45) and a button for the swarm graph.

**Graphs.** Pinch to zoom, drag to pan, tap a node: its details (trust score and why, seeders,
relay state…) and, depending on the node, *Open torrent*, *Open title*, *Copy npub* or
*Copy URL*. Node size = weight (torrents, seeders, publishers), colour = kind (one colour per
kind in every graph), a ring around a publisher = its trust score (full ring = 100). Labels
show for the subject, big nodes, the selection and its neighbours, and all of them once zoomed
in.

**Offline / older servers.** Without a server, the charts are counts of the cached torrents
for the fields the phone can answer (category, and the release attributes it parsed:
languages, resolution, source, codec, HDR, audio), without cross-filtering; activity, health
and graphs say they need the server. A server older than D47 lacks the `stats`/`graphs`
features in `/health`: the app says so and falls back the same way. While the server's search
index is still building, breakdowns answer 503 and the app shows the phone's counts.

## Colours and numbers

Categorical colours are a fixed, colour-blind-checked order (adjacent pairs ΔE ≥ 8 under CVD
simulation, ≥ 15 normal vision), with separate steps for light and dark themes
(`ui/theme/ChartColors.kt`). Donuts are used only for fields with a short fixed vocabulary
(category, resolution, movie/series/anime) and a value keeps its colour slot whatever the
filters do to its rank; everything else is one-hue bars (magnitude). Seeded/dead/no answer use
reserved status colours, always with a label. Every bar and slice has its label and number
written next to it. Numbers follow the phone's locale ("1 234 567", "12,3 k" in French).

## API

All endpoints need the bearer token. Answers are cached on the server (breakdowns 2 min,
series 1 min, health 30 s, graphs 5 min, the swarm graph 30 s since a seed check changes it).

### `GET|POST /api/v1/stats/breakdown`

`POST` body `BreakdownRequest {query: SearchQuery, fields: [..], top}`; `GET` takes
`field=` (repeatable or comma-separated), `top=` and the search query string of
`/api/v1/search` (`lang=fr&res=1080p&cat=video&min_trust=0…`).

Fields: `language`, `subtitles`, `resolution`, `source`, `codec`, `hdr`, `audio`, `channels`,
`category`, `subcategory` (the publisher's `tcat` path), `genre`, `year`, `media_kind`,
`edition`, `format`, `group`. `top` default 10, max 50.

```json
{"total": 501522, "computed_at": 1793000000,
 "breakdowns": [{"field": "language", "total": 501522,
   "counts": [{"value": "en", "count": 120345}, {"value": "fr", "count": 50211}],
   "other": 9120, "other_values": 41, "missing": 310200}]}
```

Each field is counted with every filter of `query` except its own (also for `category` and
the year range, which are counted in a pass of their own). Multi-valued fields (languages,
HDR, audio, genres) count a torrent once per value. `other` sums the values after the top
ones, `missing` counts torrents without any value. 503 `search_index_unavailable` until the
index is built (D43); the counts come from the index's facet collector, which also counts
documents without a value.

### `GET /api/v1/stats/timeseries?metric=a,b&bucket=hour|day&since=<unix>`

| Metric | Kind | Source |
|---|---|---|
| `torrents_added` | counter | new infohashes, counted by the D43 counter trigger (`torrent_counts_ins`) |
| `torrents_total` | gauge | derived: the current infohash count minus what was added after each bucket |
| `torrents_published` | counter, per day | first publication of each infohash (`liveness_schedule.born`), recomputed hourly |
| `relays_discovered` | counter | statement trigger on `relays` inserts |
| `crawl_events` | counter | events inserted by crawler passes (`daemons::crawl_pass`) |
| `liveness_checks` | counter | infohashes checked per seed-check batch |
| `db_size` | gauge | `pg_database_size`, sampled every 10 min |

Defaults: `bucket=day`; `since` = 48 h (hours) or 90 days (days), at most 31 days of hours or
10 years of days. Counter buckets without events are 0; gauge buckets without a sample are
left out. Buckets are UTC hours/days.

Storage: `stats_series (metric, bucket, value)`, one row per metric and hour (about 6 × 24
rows a day, kept 800 days; `torrents_published` holds days). On the first start after the
migration, `torrents_added` is backfilled once from `events.received_at` of each infohash's
first event (`meta.stats_since` / `stats_backfilled`); `relays_discovered`, `crawl_events`,
`liveness_checks` and `db_size` start empty (no history to rebuild them from).

### `GET /api/v1/stats/health`

`StatsHealth {torrents, liveness: LivenessStats, search_index: SearchIndexStatus, relays:
{total, enabled, ok_24h, failing, productive}, crawler: CrawlerStatus, media_linked,
attrs_missing, computed_at}`.

### Graphs: `GET /api/v1/graphs/…`

`Graph {kind, nodes: [{id, kind, label, weight, score, detail: [..]}], edges: [{source,
target, weight, kind}], truncated, note}`; edges index into `nodes`; at most 200 nodes
(`MAX_GRAPH_NODES`), `truncated` when some were left out.

| Path | Nodes | Edges |
|---|---|---|
| `wot` | trust root, the root's follows that follow a top publisher (most such follows first), the top 100 publishers (trust score + its explanation, D18) | `follows` |
| `relays` | the top 100 publishers with a NIP-65 list, their write relays (sized by the torrents of their publishers; crawler state in the details) | `writes_to` |
| `media/{id}` | the title, its qualities, its 120 most seeded releases, their languages, release groups and seasons | `quality`, `language`, `group`, `season` |
| `swarm/{infohash}` | the torrent (seeders/leechers, best source), its trackers (state of the server's checker: answering, failing, backing off, not asked yet), the DHT (BEP 33 estimate), its publishers | `tracker`, `dht`, `published` |

Without a trust root, `wot` shows the top 150 publishers and who of them follows whom (with a
`note`). Which relay delivered an event is not recorded, so the relay graph uses the relays
publishers say they write to. Per-tracker seeder counts are not stored either (D16 keeps the
maximum over trackers), so the swarm graph shows which source gave the best answer.

404 for an unknown media or infohash; servers older than D47 also answer 404, which the
client turns into "no graph".

## Performance

Production copy (501 522 infohashes, `scripts/bench-search-db.sh 1` plus the trust and relay
tables), empty cache, release build, `tests/stats_bench.rs` (10 runs, p50 / max):

| Endpoint | p50 | max |
|---|---|---|
| breakdown, 11 fields, no filter | 21.0 ms | 23.8 ms |
| breakdown, 11 fields, `lang=fr` | 14.0 ms | 18.2 ms |
| breakdown, 11 fields, fr + 1080p + video | 12.7 ms | 13.1 ms |
| breakdown, 11 fields, text "2023" | 8.9 ms | 10.4 ms |
| timeseries, all metrics, hours / days / 10 years | 5.0 / 3.5 / 7.6 ms | 10.7 / 4.3 / 8.5 ms |
| health | 35.8 ms | 73.7 ms |
| graph `wot` (no root / most-following account as root: 109 nodes) | 6.2 / 12.3 ms | 8.4 / 13.5 ms |
| graph `relays` (103 nodes) | 4.5 ms | 6.5 ms |
| graph `media` (the title with most releases: 138 nodes, 363 edges) | 41.9 ms | 44.0 ms |
| graph `swarm` | 4.5 ms | 9.9 ms |

The breakdown runs its category pass (counted without the category filter) beside the main
pass. One earlier run, before the two passes ran side by side, had a single 238 ms outlier on
the text query (p50 11.9 ms); the bench asserts the p50 of every endpoint under 200 ms.

Daemon work: the one-off backfill 1.3 s, `torrents_published` recompute 141–162 ms, gauge
sample 10 ms. The bench copy has no real `received_at` history, so its backfill is a single bucket.

On the phone, the layout (force-directed, O(n²) per step, at most 400 steps) runs on
`Dispatchers.Default` and publishes positions every 25 steps; drawing is one Canvas pass
(edges, circles, cached text layouts). Not measured on a device.

```
MAGNETAR_BENCH_DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/mag_night_stats \
  cargo test --release -p magnetar-server --test stats_bench -- --ignored --nocapture
```
