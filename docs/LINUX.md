# Running the server on Linux without Docker

`magnetar-server` is one static binary (musl, no shared libraries), so the same file runs on
any Linux distribution of its architecture: Debian/Ubuntu, Arch, Fedora, Alpine, a Raspberry
Pi OS 64-bit, a NAS shell. It needs Postgres (18 is what the tests and the installer's container use; older majors are
untested) and nothing else. For the all-in-Docker setup see [DOCKER.md](DOCKER.md); the decision is in
ARCHITECTURE.md D38.

| Architecture | Release asset | Built on |
|---|---|---|
| x86_64 (PCs, VPS, Synology x86) | `magnetar-server-x86_64-linux` | x86_64 |
| aarch64 (Raspberry Pi 3/4/5 with a 64-bit OS, arm64 VPS) | `magnetar-server-aarch64-linux` | native arm64 runner |

32-bit ARM (armv7, a 32-bit Raspberry Pi OS) is not built.

## Install

```sh
git clone https://github.com/awake-devel/magnetar && cd magnetar
sh scripts/install-linux.sh            # add --start to start it right away
```

or, without a checkout, fetch the installer of a release (it then fetches `magnetar-ctl`
from the same release and checks both against `SHA256SUMS`):

```sh
curl -fsSLO https://github.com/awake-devel/magnetar/releases/latest/download/install-linux.sh
sh install-linux.sh
```

It runs as your user, needs no root and installs nothing that starts by itself:

| Path | What |
|---|---|
| `~/.local/bin/magnetar-server` | the binary (`MAGNETAR_PREFIX` changes `~/.local`) |
| `~/.local/bin/magnetar-ctl` | start/stop script |
| `~/.local/share/magnetar/install-linux.sh` | copy of the installer, used by `magnetar-ctl update` |
| `~/.config/magnetar/server.env` | config, mode 600, with a random `MAGNETAR_API_TOKEN` |
| `~/.local/state/magnetar/server.{log,pid}` | log and pid file; librqbit state in `downloads/` beside them |
| `~/Downloads/magnetar` | server downloads (`MAGNETAR_DOWNLOAD_DIR`) |

Re-running is safe: the token, the database password and anything you edited in
`server.env` are kept, only missing keys are added. It needs `sh`, `od`, `sha256sum` and
`curl` or `wget`; bash is not required (tested with Debian's dash and Alpine's busybox ash).

### Where the binary comes from

| Setting | Effect |
|---|---|
| `MAGNETAR_BINARY=/path/to/magnetar-server` | install that file (checked to run here) |
| default | download `magnetar-server-<arch>-linux` from the latest release, verify against `SHA256SUMS` (a missing entry or mismatch aborts) |
| `MAGNETAR_VERSION=v0.2.0` | that tag instead of the latest release. Prereleases are never "latest", so they need it. It is saved to `server.env` for `magnetar-ctl update` |
| `MAGNETAR_REPO=owner/name` | a fork |

### Where the database comes from

| Case | What happens |
|---|---|
| `DATABASE_URL=postgres://user:pw@host:5432/db` set | used as is and written to `server.env`. The role must own the database; the migrations create the extension `pg_trgm`, which is trusted, so no superuser is needed |
| docker is installed and usable by you | container `magnetar-postgres` from `postgres:18`, named volume `magnetar-postgres-data`, port `127.0.0.1:54320`, restart policy `unless-stopped`, random password in `MAGNETAR_DB_PASSWORD`. Change with `MAGNETAR_DB_CONTAINER`, `MAGNETAR_DB_VOLUME`, `MAGNETAR_DB_PORT`, `MAGNETAR_PG_IMAGE` |
| neither | prints the `createuser`/`createdb` commands for a distro Postgres and stops; re-run with `DATABASE_URL=...` |

`MAGNETAR_DB=url|docker|manual` forces a case. The docker container restarts with the docker
daemon (that is docker's restart policy, not a unit we install); the server itself does not.

Distro Postgres, for example on Debian/Ubuntu:

```sh
sudo apt install postgresql
sudo -u postgres createuser --pwprompt magnetar
sudo -u postgres createdb -O magnetar magnetar
DATABASE_URL='postgres://magnetar:<password>@127.0.0.1:5432/magnetar' sh scripts/install-linux.sh
```

A Unix socket works too: `DATABASE_URL='postgres:///magnetar?host=/run/postgresql&user=magnetar'`
(peer authentication, when your login name is `magnetar`).

## Run it

```
magnetar-ctl up        # start the DB container (docker case), migrate, start, wait for /health
magnetar-ctl status    # pid, /health, DB container, tailscale serve, binary version
magnetar-ctl logs -f
magnetar-ctl token     # the API token for the app
magnetar-ctl restart   # after editing server.env
magnetar-ctl down      # stop the server (SIGTERM, SIGKILL after 30 s); DB keeps running
magnetar-ctl stop-all  # down + stop the DB container + remove the tailscale serve entry
magnetar-ctl update    # new binary from MAGNETAR_VERSION (default latest), restart if running
magnetar-ctl run       # foreground, for a supervisor or a terminal
```

The server is a background process started with `setsid nohup`: it survives closing the
terminal but not a reboot. After a reboot run `magnetar-ctl up` again, or use one of the
examples below.

## Reaching it from the phone

The server listens on `MAGNETAR_BIND`, `127.0.0.1:8787` by default: nothing but this host can
reach it. The Android app needs a URL it can reach.

**Tailscale (recommended).** With [Tailscale](https://tailscale.com) on the host and the
phone, MagicDNS and HTTPS certificates enabled for the tailnet, set in `server.env`:

```sh
MAGNETAR_TAILSCALE_SERVE='1'
# MAGNETAR_TAILSCALE_HTTPS_PORT='443'   # if 443 is taken by another `tailscale serve`
```

`magnetar-ctl up` then runs `tailscale serve --bg --https=443 http://127.0.0.1:8787` and prints
the URL, e.g. `https://myhost.tailnet-name.ts.net`. Put that and the token in the app.
`tailscale serve` without root needs `sudo tailscale set --operator=$USER` once. The serve
entry is kept by tailscaled across reboots; `stop-all` removes it.

**LAN only.** `MAGNETAR_BIND='0.0.0.0:8787'` and the app's server URL
`http://<host-ip>:8787`. Every endpoint except `/health` needs the token, but it travels in
clear text: use this only on a network you trust, and firewall the port from the internet.

**Your own reverse proxy** (Caddy, nginx, Traefik): keep the loopback bind and proxy HTTPS
to `127.0.0.1:8787`; the `/relay` path is a WebSocket, so the proxy must pass `Upgrade`.

## Configuration

Everything is in `server.env` (values single-quoted, it is sourced by `sh`). The full list is
in [CONFIG.md](CONFIG.md). Useful on an always-on machine:

| Variable | Suggestion |
|---|---|
| `MAGNETAR_CRAWL_INTERVAL_SECS` | `0` for continuous crawling (D32); the default 900 is gentler on a Pi |
| `MAGNETAR_DOWNLOADS` | `false` to not start the librqbit engine at all |
| `MAGNETAR_NIP66` | `false` to skip NIP-66 relay discovery |
| `MAGNETAR_ROOT_NPUB` | your npub, the root of the web of trust |
| `MAGNETAR_NTFY_URL`, `MAGNETAR_NTFY_TOPIC` | saved-search notifications |

`magnetar-ctl` also reads `MAGNETAR_ENV_FILE`, `MAGNETAR_STATE_DIR` and `MAGNETAR_SERVER_BIN`
from its own environment, so several instances can live side by side.

## Starting at boot (optional, examples only)

Nothing below is installed by the scripts. Adapt one if you want autostart.

cron, as your user (`crontab -e`); the DB container already comes back with docker:

```
@reboot sleep 30 && $HOME/.local/bin/magnetar-ctl up >>$HOME/.local/state/magnetar/boot.log 2>&1
```

systemd user unit, `~/.config/systemd/user/magnetar.service`, then
`systemctl --user enable --now magnetar` and `loginctl enable-linger $USER`:

```ini
[Unit]
Description=Magnetar server
After=network-online.target

[Service]
ExecStart=%h/.local/bin/magnetar-ctl run
Restart=on-failure

[Install]
WantedBy=default.target
```

OpenRC (Alpine), `/etc/init.d/magnetar`, as root, for user `alice`:

```sh
#!/sbin/openrc-run
command=/home/alice/.local/bin/magnetar-ctl
command_args=run
command_user=alice
command_background=true
pidfile=/run/magnetar.pid
output_log=/home/alice/.local/state/magnetar/server.log
error_log=/home/alice/.local/state/magnetar/server.log
depend() { need net; after docker postgresql; }
```

Synology DSM (UNVERIFIED): Control Panel > Task Scheduler > Create > Triggered Task > Boot-up, user = your
account, command `/var/services/homes/<you>/.local/bin/magnetar-ctl up`. On a Synology the
Docker route (Container Manager, [DOCKER.md](DOCKER.md)) is usually simpler.

## Build the binary yourself

```sh
scripts/build-linux.sh x86_64     # on an x86_64 host
scripts/build-linux.sh aarch64    # on an arm64 host (arm64 VPS or CI runner; UNVERIFIED on a Pi, where a full build is slow and memory-hungry)
```

Output: `dist/magnetar-server-<arch>-linux`, which the script checks is static (no ELF
interpreter, no `NEEDED` entries) and runs with `--version`. The build is native only: the
target must be the host's architecture, nothing is emulated.

- **Default (`--docker`)**: builds inside `rust:1.99-alpine` (`RUST_IMAGE` to change), where
  musl is the native libc, so ring and aws-lc (librqbit, D28) compile with Alpine's gcc and
  link statically. Only docker is needed on the host. Objects go to `target/linux-musl/`.
- **`--native`**: the host's cargo with `<arch>-unknown-linux-musl`. On Alpine:
  `apk add rust cargo musl-dev` (or rustup). On Debian/Ubuntu: rustup plus
  `apt install musl-tools`; the script then uses `musl-gcc` as the C compiler.
  **UNVERIFIED:** only the docker mode was run (x86_64, 2026-10-25); the aarch64 binary is
  built by CI and has not been run here.

Then install it with `MAGNETAR_BINARY=dist/magnetar-server-x86_64-linux sh scripts/install-linux.sh`.

## Uninstall

```sh
magnetar-ctl stop-all
rm -f ~/.local/bin/magnetar-server ~/.local/bin/magnetar-ctl
rm -rf ~/.local/share/magnetar
docker rm magnetar-postgres && docker volume rm magnetar-postgres-data   # deletes the index
rm -rf ~/.config/magnetar ~/.local/state/magnetar                        # config, token, logs
```
