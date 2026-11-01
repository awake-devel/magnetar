Changes since v0.3.1 (not released yet).

## What's new

### Security: hostile peers, trackers and relays can no longer exhaust memory or reach your LAN (D110–D113)

A security audit (8 agents, every finding re-checked by a second one, then two rounds of
fix and adversarial review) found ways for anyone on the internet to crash the server or a
worker, and for token holders to abuse the API. Every fix has a regression test that fails on
the old code. **Update the server and every worker.**

**Anyone on the DHT, any tracker or Nostr relay** (no token needed):

- **Torrent metadata is bounded before it is built** (D110). A crafted BEP 52 file tree could
  make one 16 MiB info dict expand to gigabytes. Output is now priced in real bytes and
  capped, both absolutely and relative to the dict's size; names and paths are cut to 1 KiB;
  the release parser and its title folding are linear (a 1 MB hostile name took 26 s to fold,
  now well under a second); bencode lengths with a sign (`-0:`) are refused. One row the database
  refuses no longer fails a whole batch of new torrents.
- **Every remote body has a size cap** (D111): tracker scrapes, Nostr relay messages (4 MiB)
  and pages (charged by decoded size, tag counts capped), GeoIP and TMDB downloads, worker
  hello text. An honest oversized event is dropped instead of failing the relay.
- **No outbound connection to private addresses** (D112). Tracker URLs, redirects, DHT
  `nodes`, relay host names and download peers that resolve to the LAN, localhost, the
  tailnet (100.64/10), IPv6 ULA or link-local are refused at connect time, in any spelling
  (`127.1`, `0x7f000001`, `[::ffff:…]`, trailing dots). Your own public addresses are refused
  too (learned from the DHT, interfaces, and the new `MAGNETAR_OWN_ADDRS`). Tracker
  redirects are followed only after the same check. `tr=` trackers pointing at local
  addresses are removed from magnets before a download.

**Token holders** (a worker or the phone):

- **Workers only act on hashes they hold** (D113): uploads and block reports for hashes
  another worker or the server holds are ignored; a worker's block no longer stops Nostr
  ingest of that torrent. A worker can still store dicts it made up for hashes it triaged
  itself, within `MAGNETAR_WORKER_TRIAGE_NEW_PER_HOUR` (100 000), and every stored dict
  records which worker supplied it, so a leaked token's torrents can be removed
  (docs/WORKERS.md §7).
- Upload routes take one of a few slots (503 + `Retry-After` when full); the other worker
  routes accept 1 MiB at most. Forced seed checks from the app have a cooldown and a rate,
  share the tracker backoff and are capped at 30 trackers. The API no longer returns raw
  database errors, and the `/relay?token=` query is never logged. `/relay` subscriptions are
  capped (filters, values per REQ, tag value length).
- Workers stop retrying requests the server will always refuse (400, 413), and retry
  408/429/5xx with backoff. `docker/worker.env` is gitignored and `compose.worker.yml` reads it.

**What you may notice:** search pages past 10 000 results return 400 (the app shows a
message); trackers on private addresses no longer get scraped; a few malformed torrents are
now reported as `too_complex`.

### The crawler names itself on every protocol (D115)

DHT queries (`v` = `MG` + version, now also the seed checker's), peer ids (`-MG0320-` for 0.3.2,
was stuck at `-MG0300-`), the peer extension handshake (`Magnetar 0.3.2`) and the HTTP and
relay User-Agent (`magnetar/0.3.2 (+https://github.com/awake-devel/magnetar)`) all carry the same
honest name, from the crate version. It never impersonates another client
(docs/DHT.md §3.3).

### Workers crawl as fast as their query rate allows (ARCHITECTURE.md D114)

In aggressive mode the number of peer collections running at once was fixed at 64. Each one
waits about 8.6 s for its DHT walk, so any `MAGNETAR_DHT_QPS` above ~180 was never reached: on
two workers a rate ramp up to 1300 q/s still sent only ~160–180 q/s. With
`MAGNETAR_DHT_PEER_LOOKUPS` unset, aggressive mode now runs 0.4 collections per q/s (8–4096;
80 at the default 200 q/s). Stored metadata grows about linearly with collectors until the line
saturates (docs/benchmarks/2026-10-31-worker-collectors). An explicit value still wins; polite
mode keeps 8.

### Nightly database backups (docs/DOCKER.md "Backups")

`scripts/backup.sh` dumps the database (`pg_dump -Fc`, consistent while the server runs) to a
local directory or to another machine over ssh, and keeps the newest 7. A failed dump never
replaces a good one. `packaging/magnetar-backup.{service,timer}` runs it every night as a
systemd user unit. A 9.5 GB database dumps to 1.7 GB in 4.5 minutes.
