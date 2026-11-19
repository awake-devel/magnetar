Changes since v0.3.2 (not released yet; pre-release `v0.3.2-redeploy1` carries everything up to D146).

## What's new

### Upgrade note: the database migration has no way back (D144)

The first start applies migration `20261112000000_enrich_due`. After it, an older image or
binary refuses to start (sqlx reports the migration as missing). Take a `pg_dump` and check it
with `pg_restore -l` before the first start, and roll back only by restoring that dump or to a
build of this version.

### One limit on outbound connections for the whole process (D150)

Every outbound connection other than the DHT (Nostr relays, tracker scrapes, TMDB, IMDb,
DB-IP, ntfy) now goes through one destination budget: at most `MAGNETAR_NET_DESTS` (1024)
distinct public addresses per 5 minutes and `MAGNETAR_NET_FLOWS` (20) new connections per
second, halved by the router health brake when the line degrades. Over the limit a connection
waits briefly, then is skipped (error `budget`) without counting as a tracker or relay failure;
the log summarises waits and refusals every 10 minutes. LAN and
tailnet addresses are not counted. `MAGNETAR_NET_DESTS=0` turns it off.

### Nostr backfill no longer stops at a burst (D151)

When a relay held more torrents in one second than it returns per page, the crawler took the
repeated page for the end of the history and marked the backfill done, leaving everything
older unread. It now steps past that second and continues.

### The app and the web page are ready for new sources (D152)

A torrent whose source the client does not know yet is shown as "Other source", with no trust
badge and no publisher card, instead of being presented as a Nostr torrent. Nothing changes
for today's Nostr and DHT results.

### A background loop that stops is reported (D147)

Every background loop (crawler, seed checker, router health probe, enrichment, …) now runs
under a name. If one exits or panics while the server runs, the log says which one, at ERROR,
and `GET /health` answers `"status": "degraded"` instead of `"ok"`. Nothing is restarted
automatically, and the Docker healthcheck still looks only at the database.

### Searches no longer hang while the search index rebuilds (D148)

When the search index is off or still building, searches fall back to Postgres. Those queries
now stop after 15 s and answer `timeout` (504), so a slow search cannot hold the database.

### Seed checks are always paced and braked (D146)

With the DHT crawler off and `MAGNETAR_LIVENESS_DHT=false`, the seed checker used to scrape
trackers with no shared pace and no router health probe. Every process that runs seed checks
now has both (at `MAGNETAR_LIVENESS_DHT_QPS` with the crawler off), so tracker scrapes slow
down when the router saturates. DHT walks still need `MAGNETAR_LIVENESS_DHT`. The probe makes a
TCP connect to `MAGNETAR_DHT_PROBE` (default `1.1.1.1:443,9.9.9.9:443`) every 2 s; `off`
disables it.

### Workers say hello again after a server restart (D146)

The server keeps worker hellos in memory. After a restart it now asks each worker for a new
hello in its heartbeat answer, and until one arrives an `oversize` report is never final (a
4 MiB worker such as a Pi used to close torrents for good that a larger worker could fetch).
Old workers keep working with the new server; update them to get the re-hello.

### Enrichment finds due torrents through a queue (D144)

The IMDb/TMDB enricher no longer anti-joins two 2.4 M-row tables for every batch: triggers
keep an `enrich_due` queue. Measured before: 2.6 s per batch of 200, up to 47 s under disk
load.

### Postgres write settings in `compose.yml` (D145)

`POSTGRES_MAX_WAL_SIZE` (default `8GB`) and `POSTGRES_SHARED_BUFFERS` set the `db` service's
`max_wal_size` and `shared_buffers`, against requested checkpoints on slow disks.

### Fixes

- Header probes (`sniff`, MP4): a crafted fragmented file could cost ~1.7e10 loop steps per
  parse on a worker; runs without per-sample fields are now counted at once (D146).
- docs/DOCKER.md recommends `tailscale serve --https=8443` and checking `tailscale serve
  status` first: serving on a used port replaces what was there (D146).
- README brought up to date with v0.3.2 (workers, DHT crawler, enrichment, D137–D145).
- The web session cookie now opens only the routes the web page uses. `/api/v1/relays`,
  `/api/v1/trust/*` and `/api/v1/overrides/*` need the bearer token, like any route added
  later (D153).
