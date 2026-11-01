Changes since v0.3.1 (not released yet).

## What's new

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
