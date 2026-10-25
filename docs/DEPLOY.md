# Deploying the Magnetar server

The server is `magnetar-server` plus a Postgres 18 database. It crawls Nostr relays all the
time, keeps the full index, and is what the Android app syncs to and searches through. The
app also works without it (offline crawl and search on the phone), but the server is the
always-on part, so put it on a machine that stays on.

There are two ways to run it. Both are configured only with environment variables (an `.env`
file), and neither installs systemd units, timers or udev rules.

| | Docker Compose (recommended) | Native Linux binary |
|---|---|---|
| What you get | `magnetar-server` and `postgres:18` as two containers, restarted by Docker | one static binary, your own Postgres 18 |
| Architectures | image `ghcr.io/awake-devel/magnetar` for `linux/amd64` and `linux/arm64` | `magnetar-server-x86_64-linux`, `magnetar-server-aarch64-linux` from the release |
| Updates | `docker compose pull && docker compose up -d` | replace the binary, restart |
| Guide | [docs/DOCKER.md](DOCKER.md) | [docs/LINUX.md](LINUX.md) |

`serve` applies the database migrations itself at startup, so an update needs no extra step.

## Which one for which device

| Device | Use | Notes |
|---|---|---|
| Linux PC or home server (x86_64 or arm64) | Docker Compose, or the native binary if you already run Postgres | Any distribution with Docker Engine and the compose plugin |
| Synology NAS, x86_64 models | Docker Compose through **Container Manager** (DSM 7.2+): Project > Create, point it at a folder with `compose.yml` and `.env` | Keep the data on a volume, not on the system partition. ARM Synology models are not a target: the arm64 image may run on the aarch64 ones but this is **UNVERIFIED**, and 32-bit ARM has no image |
| Raspberry Pi 4 / 5 | Docker Compose, or the native `aarch64` binary | Needs a **64-bit** OS (`uname -m` prints `aarch64`). Put Postgres on an SSD rather than the SD card: the crawler writes constantly. Not yet run on a real Pi (**UNVERIFIED**) |
| VPS | Docker Compose | Never publish port 8787 on the public interface without TLS: use Tailscale or a reverse proxy with HTTPS (below) |
| Mac (Intel or Apple Silicon) / Windows | Docker Desktop (WSL 2 backend on Windows), Docker Compose | Docker pulls the right architecture by itself. Only useful while the machine is on |

The phone itself is no longer a server target. The Termux setup was removed (ARCHITECTURE.md
D39): Android kills long-running background processes, and a phone that sleeps is a poor
crawler.

## Quick start with Docker Compose

```sh
git clone https://github.com/awake-devel/magnetar && cd magnetar
cp .env.example .env
$EDITOR .env                  # set the values marked required, e.g. MAGNETAR_API_TOKEN=$(openssl rand -hex 32)
docker compose pull           # or: docker compose up -d --build   to build the image from source
docker compose up -d
curl -s http://127.0.0.1:8787/health
```

Releases after v0.1.0 also carry `compose.yml` and `.env.example` as assets, so a git
checkout is optional.
[docs/DOCKER.md](DOCKER.md) has every variable, the volumes, `selftest`, logs, backups and
upgrades. By default the API is published on `127.0.0.1:8787` only (`MAGNETAR_PUBLISH`
changes that). The image tag follows `MAGNETAR_VERSION` (default `latest`, which only ever
points at a non-prerelease; `edge` is the newest `main` build).

## Native binary

Download `magnetar-server-<arch>-linux` and `SHA256SUMS` from the release, check them with
`sha256sum -c --ignore-missing SHA256SUMS`, and follow [docs/LINUX.md](LINUX.md) for Postgres,
the env file, and starting it in the foreground or the background. To run it at boot, adapt
one of the examples there yourself; Magnetar installs no service.

## Reaching the server from the phone

Keep the server on loopback and put something with HTTPS in front of it. Every request
except `GET /health` needs the bearer token, and the token must not cross a network in clear.

### Tailscale (recommended)

Install Tailscale on the server (on Synology: Package Center > Tailscale) and on the phone,
enable MagicDNS and HTTPS certificates for the tailnet (admin console, DNS page), then on the
server:

```sh
tailscale serve --bg 127.0.0.1:8787     # https://<machine>.<tailnet>.ts.net -> 127.0.0.1:8787
tailscale serve status
```

In the app, set the server URL to `https://<machine>.<tailnet>.ts.net`. Nothing is exposed
outside the tailnet. Tailscale is optional: Magnetar does not depend on it.

### Reverse proxy with TLS

Any proxy works if it forwards WebSocket upgrades (the phone syncs over `/relay`). Caddy
handles certificates and WebSockets by itself:

```
magnetar.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

nginx needs the upgrade headers for `/relay`:

```nginx
location / {
    proxy_pass http://127.0.0.1:8787;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 300s;
}
```

On Synology, Control Panel > Login Portal > Advanced > Reverse Proxy does the same; add the
**WebSocket** custom header to the rule.

### Plain HTTP on a trusted LAN

Possible, not recommended. Publish the port on the LAN address (`MAGNETAR_PUBLISH` for
Docker, `MAGNETAR_BIND` for the native binary, e.g. `192.168.1.20:8787`) and use
`http://192.168.1.20:8787` in the app. The app accepts it and shows a warning: anyone on that
network can read the token and the traffic. Plain `http://` to a Tailscale address
(`100.x.y.z` or a `*.ts.net` name) does not warn, because WireGuard already encrypts it.

## App setup

1. Install `magnetar-<tag>.apk` from the release (allow "install unknown apps" for your
   browser or file manager).
2. Settings > Server URL: `https://<machine>.<tailnet>.ts.net` (or your proxy's URL). Leave it
   empty to stay offline.
3. API token: the `MAGNETAR_API_TOKEN` from the server's `.env`.
4. Root npub (optional): your own npub, so the web of trust starts from your follows.
5. Tap **Test connection**. The Status screen then shows the server as reachable.

The app syncs what it crawls up to the server once an hour while the phone is on Wi-Fi and
charging, or when you tap Sync now.
