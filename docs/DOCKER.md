# Running the server with Docker

`compose.yml` at the repo root runs two containers: `db` (`postgres:18`) and `magnetar`
(`ghcr.io/awake-devel/magnetar`, built from the root `Dockerfile`). This works the same on a
Linux PC or VPS, a Synology NAS (x86_64), a Raspberry Pi with a 64-bit OS and Docker
Desktop on macOS or Windows. The image is meant to be published for `linux/amd64` and
`linux/arm64` by CI (`.github/workflows/docker.yml`, D39), but **nothing is published yet**:
until the first release, build it from a full checkout (below).
Design notes are in ARCHITECTURE.md D37, and every variable is in docs/CONFIG.md.

Nothing is installed on the host apart from Docker: no systemd unit, timer or udev rule.
Docker's `restart: unless-stopped` brings both containers back after a crash or a reboot
(as long as the Docker daemon itself starts at boot, which is the default for Docker Engine
packages, Synology and Docker Desktop with "Start when you sign in").

## Quick start

```sh
git clone https://github.com/awake-devel/magnetar && cd magnetar
cp .env.example .env && chmod 600 .env   # holds the DB password and the API token
# edit .env: POSTGRES_PASSWORD=$(openssl rand -hex 24), MAGNETAR_API_TOKEN=$(openssl rand -hex 32)
docker compose build                   # until an image is published (see below)
docker compose up -d
docker compose ps                      # both "healthy" after ~30 s
curl http://127.0.0.1:8787/health      # {"status":"ok","version":"0.1.0","db":true}
```

**Building** needs the whole checkout (`Dockerfile` and `crates/`), not just `compose.yml`.
On this 12-thread laptop the first build took about 15 minutes and a rebuild after a source
change about 4.5, since cargo-chef keeps the dependency layer; expect far longer on a Pi
(not measured).

**Pulling** (planned): once CI publishes `ghcr.io/awake-devel/magnetar`, `docker compose up -d`
pulls it (checked: compose pulls a tag that exists and builds only when the pull fails) and
the build step is unnecessary; then `compose.yml`, `.env` and `docker/` are all a
host needs. CI tags `X.Y.Z` and `latest` from a release tag `vX.Y.Z` (a prerelease such as
`v0.1.0-rc1` gets only its own tag and never moves `latest`) and `edge` from `main`. Until
then a plain `up -d` fails to pull and falls back to building, which only works in a full
checkout. Check with `docker manifest inspect ghcr.io/awake-devel/magnetar:latest`.

Without `POSTGRES_PASSWORD` or `MAGNETAR_API_TOKEN` in `.env`, compose stops before
creating anything and prints which one is missing.

Point the Android app at the server: **Server URL** `http://<host>:8787` (or the Tailscale
HTTPS name below) and **API token** = `MAGNETAR_API_TOKEN`.

### Exposure

The API is published on `127.0.0.1:8787` only, so by default nothing outside the host can
reach it. Choose one of:

- **Tailscale** (recommended for a phone): HTTPS with a real certificate, reachable only from
  your tailnet. Either run `tailscale serve --bg --https=443 http://127.0.0.1:8787` on the
  host, or use the sidecar profile below.
- **LAN**: `MAGNETAR_PUBLISH=0.0.0.0:8787` (or the host's LAN address). Plain HTTP: the
  token travels in clear, so only on a network you trust.
- **A reverse proxy** you already run (Caddy, Traefik, nginx, Synology's own) in front of
  `127.0.0.1:8787`. `/relay` is a WebSocket, so the proxy must pass `Upgrade` headers.

Inside the container the server listens on `0.0.0.0:8787` (`MAGNETAR_BIND` is set in the
image). Do not change that: the published port decides what the host exposes.

### Environment

`.env` is read by compose itself, but the magnetar container only receives the server
variables that `compose.yml` lists by name under `magnetar.environment` (each one is left out
when unset in `.env`). So `TS_AUTHKEY` and `POSTGRES_PASSWORD` never reach the server process
(`DATABASE_URL` carries the password). To pass a variable that is not listed, add it in
`compose.override.yml`:

```yaml
services:
  magnetar:
    environment:
      MAGNETAR_SOMETHING: ${MAGNETAR_SOMETHING:-}
```

## What lives where

| Data | Where | Back it up? |
|---|---|---|
| Postgres (events, torrents, trust, settings) | volume `<project>_db-data` → `/var/lib/postgresql` | yes, with `pg_dump` (below) |
| Downloads | `/data/downloads` in volume `<project>_magnetar-data`, or `MAGNETAR_DATA` | your call |
| librqbit session (resumes downloads) | `/data/state/magnetar/downloads` (`XDG_STATE_HOME=/data/state`) | no, it is rebuilt |
| Search index (D43, docs/SEARCH.md) | `/data/state/magnetar/search-index` | no, rebuilt from Postgres in the background when missing (searches use Postgres meanwhile) |
| Tailscale node state | volume `<project>_tailscale-state` | no, re-auth instead |

`<project>` is the directory name unless you pass `-p` or set `COMPOSE_PROJECT_NAME`.

To keep downloads in a host folder, set `MAGNETAR_DATA=/path/to/folder` and make the
container user own it. The image runs as uid/gid `10001`; set `MAGNETAR_UID`/`MAGNETAR_GID`
to the folder owner's ids (`id -u`, `id -g`) instead of changing ownership.

## Day to day

```sh
docker compose logs -f magnetar                      # RUST_LOG=info by default
docker compose exec magnetar magnetar-server selftest  # DB, DNS, TLS, UDP from inside
docker compose run --rm magnetar crawl-once --seconds 60
docker compose stop / start / down                    # down keeps the volumes
```

`crawl-once` and `selftest` share the server's environment; the JSON report goes to stdout.
`serve` (the default command) applies database migrations before it listens, so the first
start and upgrades need no separate `migrate` step.

Heavy features can be switched off in `.env`: `MAGNETAR_DOWNLOADS=false` (no librqbit
engine), `MAGNETAR_NIP66=false`, a longer `MAGNETAR_CRAWL_INTERVAL_SECS` or a lower
`MAGNETAR_CRAWL_MAX_RELAYS` for small hardware. `MAGNETAR_CRAWL_INTERVAL_SECS=0` (continuous)
is fine on an always-on box. Seed checks (docs/LIVENESS.md) can be slowed with a lower
`MAGNETAR_LIVENESS_DHT_CONCURRENCY` or `MAGNETAR_LIVENESS_TRACKER_RPS`.

### Resource limits

None are set by default. `compose.yml` has commented examples for both services; uncomment
and adjust them (or put them in `compose.override.yml`) to keep Magnetar from competing with
other services on a NAS or Pi:

```yaml
services:
  magnetar:
    cpus: "2.0"      # at most two cores
    mem_limit: 2g    # killed and restarted (restart: unless-stopped) above this
  db:
    cpus: "2.0"
    mem_limit: 1g
```

Rough needs measured on a production-sized copy (~0.5 M infohashes): the server averages well
under one core (seed checks ~3 % of a core while clearing a backlog, crawling and enrichment
in bursts); the first search-index build and IMDb dataset imports use several cores for a few
minutes, and a CPU limit only makes them take longer. Memory: about 0.5 GB for the server in
steady state (production, 2026-10-26), more during the index build and dataset imports, so
leave headroom above `mem_limit: 1g`. Postgres also used about 0.5 GB; it caches what it is
allowed to, and `mem_limit` there mostly bounds the page cache it gets.

## Updates

```sh
docker compose pull          # once published: ghcr.io/awake-devel/magnetar:${MAGNETAR_VERSION:-latest}
docker compose up -d         # recreates only what changed; migrations run at start
docker image prune           # optional: drop the old image
```

Pin a release with `MAGNETAR_VERSION=0.1.0` in `.env` to update only when you choose. When
you build from source: `git pull && docker compose build && docker compose up -d`.

`postgres:18` follows Postgres 18 minor releases on `pull`, which are safe in place. A new
**major** version (19) needs a dump and restore (or `pg_upgrade`); compose will never do that
by itself because the tag pins the major.

## Backups

The database is the only thing that matters; everything else is re-crawled. A logical dump
is consistent while the server runs:

```sh
docker compose exec -T db pg_dump -U magnetar -Fc magnetar > magnetar-$(date +%F).dump
```

Restore into a fresh stack (`docker compose up -d db` first, magnetar stopped):

```sh
docker compose stop magnetar
docker compose exec -T db pg_restore -U magnetar -d magnetar --clean --if-exists < magnetar-2026-10-25.dump
docker compose start magnetar
```

Copying the `db-data` volume is also a valid backup, but only with `db` stopped.

## Using an existing Postgres container or volume

**Different server.** Set `DATABASE_URL` in `.env` to that server and start only magnetar:
`docker compose up -d --no-deps magnetar`. Postgres 18 with contrib is expected: the first
migration runs `CREATE EXTENSION IF NOT EXISTS pg_trgm` (a trusted extension, so the owner of
the database may create it). The host running Docker itself is `host.docker.internal` on
Docker Desktop; on Linux add `extra_hosts: ["host.docker.internal:host-gateway"]` to the
magnetar service in `compose.override.yml`.

**An existing `postgres:18` volume.** If you already run Postgres in a plain container, for
example

```sh
docker run -d --name magnetar-db -v magnetar-db-data:/var/lib/postgresql \
  -e POSTGRES_USER=magnetar -e POSTGRES_DB=magnetar -e POSTGRES_PASSWORD=... postgres:18
```

compose can take over its volume instead of creating a new one:

1. Put the **same** password, user and database into `.env` (`POSTGRES_PASSWORD`,
   `POSTGRES_USER`, `POSTGRES_DB`). On an existing data directory Postgres ignores the
   `POSTGRES_*` variables, so a different password only makes the login fail.
2. Create `compose.override.yml` next to `compose.yml` (compose reads it automatically):

   ```yaml
   volumes:
     db-data:
       external: true
       name: magnetar-db-data
   ```

3. Stop whatever used the volume before: `docker stop magnetar-db` and the old
   `magnetar-server` process. **Never** let two Postgres servers open one data directory.
4. Take a dump first (`docker exec magnetar-db pg_dump -U magnetar -Fc magnetar > before.dump`),
   then `docker compose up -d`. Check `docker compose logs db`: it must say
   `Database directory appears to contain a database; Skipping initialization`.
5. Once it works, remove the old container (`docker rm magnetar-db`). The volume stays: it is
   external, so `docker compose down -v` will not delete it either.

The volume must have been mounted at `/var/lib/postgresql` (the postgres:18 layout,
`PGDATA=/var/lib/postgresql/18/docker`). A volume from an older setup mounted at
`/var/lib/postgresql/data` holds a pre-18 cluster: dump and restore instead.

## Optional Tailscale sidecar

The `tailscale` service is in the compose profile `tailscale`, so it only starts when asked:

```sh
# .env: TS_AUTHKEY=tskey-auth-...  (admin console → Settings → Keys), TS_HOSTNAME=magnetar
docker compose --profile tailscale up -d
```

It joins your tailnet as `TS_HOSTNAME` in userspace mode (no `/dev/net/tun`, no extra
capabilities, so it also runs on Synology and Docker Desktop) and serves
`https://<TS_HOSTNAME>.<tailnet>.ts.net` → `http://magnetar:8787` over the compose network,
configured by `docker/tailscale-serve.json`. HTTPS certificates must be enabled for the
tailnet (admin console → DNS → HTTPS Certificates). Funnel (public internet) is off. The node
state lives in the `tailscale-state` volume, so the auth key is only used on the first start.
Only the sidecar receives `TS_AUTHKEY` (the magnetar container gets just the server variables
listed in `compose.yml`). Use a one-off (non-reusable) key and delete it from `.env` once the
node has joined.
With the sidecar you can leave `MAGNETAR_PUBLISH` at `127.0.0.1:8787`.

If the host already runs Tailscale, skip the sidecar and use the host's
`tailscale serve --bg --https=443 http://127.0.0.1:8787` instead.

**UNVERIFIED:** the sidecar was checked with `docker compose --profile tailscale config` only;
it has not been run against a real tailnet.

## DHT crawler ports (overlay planned)

The DHT crawler (docs/DHT.md, D49–D62) exists since stage 1 and is off by default
(`MAGNETAR_DHT=off`); the default `compose.yml` publishes nothing new. With `MAGNETAR_DHT` set
and no published UDP ports, it still crawls (answers to its own queries come back through the
NAT), but unsolicited queries cannot reach it, so passive capture finds almost nothing. In aggressive mode it listens on UDP ports (default 6891–6894),
which an overlay publishes:

```sh
docker compose -f compose.yml -f docker/compose.dht.yml up -d
```

UPnP from inside a container needs two variables, because SSDP multicast does not leave
Docker's bridge network and the container only knows its bridge address:
`MAGNETAR_DHT_UPNP_GATEWAY=<router LAN address>` and `MAGNETAR_DHT_UPNP_LOCAL_IP=<this host's
LAN address>`. Only UDP is mapped; the crawler never accepts incoming TCP connections. The
overlay and these variables arrive with the UPnP stage; until then nothing here is active.

## Platform notes

### Synology NAS (DSM 7.2+, x86_64 models)

Until an image is published, the NAS has to build it: copy the **whole checkout** (or `git
clone` over SSH) to a folder such as `/volume1/docker/magnetar`, then over SSH in that folder
`sudo docker compose build && sudo docker compose up -d`, or in **Container Manager → Project
→ Create** pick that folder (it should find `compose.yml` and build). Once the image is
published (planned, D39), `compose.yml`, a filled-in `.env` (`chmod 600`) and `docker/` are
enough. To put downloads in a shared folder, set `MAGNETAR_DATA=/volume1/downloads/magnetar`
and `MAGNETAR_UID`/`MAGNETAR_GID` to the user that owns it (`id <user>` over SSH).
ARM-based Synology models (`aarch64`) build or pull for arm64 like a Pi; 32-bit ARM models
are not supported. **UNVERIFIED** on a real DSM.

### Raspberry Pi (64-bit Raspberry Pi OS / Debian / Ubuntu, Pi 4 or 5)

Install Docker Engine (`curl -fsSL https://get.docker.com | sh`), then the quick start. A
32-bit OS (`armv7l` in `uname -m`) is not supported. Prefer an SSD over the SD card for the
Postgres volume (it writes constantly while crawling). Until an arm64 image is published
(planned: CI builds it on GitHub's native arm64 runners, D39), the quick start builds on the
Pi itself, which takes long; afterwards `up -d` pulls the arm64 variant automatically.
**UNVERIFIED** on arm64: this laptop is x86_64 without emulation, so neither the arm64 build
nor the image has been run here.

### Docker Desktop (macOS, Windows)

Same commands in a terminal (PowerShell works). `docker compose build` builds for the
machine's own architecture (linux/arm64 on Apple silicon, linux/amd64 on Intel Macs and
Windows); a published image would be picked the same way. Docker Desktop's VM sleeps with
the laptop, so crawling pauses too. To reach the server from a phone on the same Wi-Fi, set
`MAGNETAR_PUBLISH=0.0.0.0:8787` and allow the port in the OS firewall, or use Tailscale on
the host. **UNVERIFIED** on macOS and Windows.

## Image details

- Multi-stage: `rust:1.99.0-slim-trixie` builds `magnetar-server` (cargo-chef caches the
  dependency layer, so a source-only change rebuilds only our crates), the runtime is
  `debian:trixie-slim` with the binary at `/usr/local/bin/magnetar-server`. Both are pinned by
  index digest.
- Runs as the unprivileged user `magnetar` (uid/gid 10001), home and volume `/data`.
- Size on this laptop (linux/amd64): 151 MB unpacked, 40.5 MB compressed; the stripped
  binary is 23 MB.
- Logs are plain text (`NO_COLOR=1`); `RUST_LOG` controls the level.
- `ENTRYPOINT ["magnetar-server"]`, `CMD ["serve"]`, `EXPOSE 8787`.
- `HEALTHCHECK` calls `GET /health` with bash's `/dev/tcp` and requires `"db":true`, so
  "healthy" means the server is up **and** reaches Postgres.
- CA certificates are included for HTTPS trackers (librqbit) and future metadata lookups.
- Build for another architecture natively on that architecture; the Dockerfile does not
  cross-compile.
