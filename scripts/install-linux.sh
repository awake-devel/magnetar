#!/bin/sh
# Install the Magnetar server on a Linux host, as your user (no root, no systemd units).
# Idempotent: re-running keeps the API token, the database password and every value you
# edited in the env file; it only adds missing keys and (with --update) replaces the binary.
#
#   sh install-linux.sh                 install or repair
#   sh install-linux.sh --update        also replace an installed binary
#   sh install-linux.sh --start         then run `magnetar-ctl up`
#   sh install-linux.sh --print-token
#
# Binary (first match wins):
#   MAGNETAR_BINARY=/path/magnetar-server   install a local file (e.g. from scripts/build-linux.sh)
#   default    download magnetar-server-<arch>-linux from a GitHub release and check it
#              against the release's SHA256SUMS
#     MAGNETAR_VERSION=v0.2.0   a tag instead of the latest release (needed for prereleases)
#     MAGNETAR_REPO=owner/name  default awake-devel/magnetar
#     MAGNETAR_RELEASE_BASE=URL a mirror holding the release assets (URL/<asset>)
#
# Database (first match wins):
#   DATABASE_URL=postgres://...   use an existing Postgres (database and role must exist)
#   docker on PATH and usable     run postgres:18 as container MAGNETAR_DB_CONTAINER
#                                 (magnetar-postgres) with named volume MAGNETAR_DB_VOLUME
#                                 (magnetar-postgres-data) on 127.0.0.1:MAGNETAR_DB_PORT (54320),
#                                 restart policy unless-stopped
#   otherwise                     print the commands for a distro Postgres, then stop
#   MAGNETAR_DB=docker|url|manual forces one of them
#
# Layout: MAGNETAR_PREFIX (default ~/.local): bin/magnetar-server, bin/magnetar-ctl,
# share/magnetar/install-linux.sh. Config: ${XDG_CONFIG_HOME:-~/.config}/magnetar/server.env
# (chmod 600). Values first written from the environment when set: MAGNETAR_BIND
# (127.0.0.1:8787), MAGNETAR_DOWNLOAD_DIR (~/Downloads/magnetar), MAGNETAR_ROOT_NPUB,
# MAGNETAR_TAILSCALE_SERVE (0). See docs/LINUX.md.
set -eu

REPO=${MAGNETAR_REPO:-awake-devel/magnetar}
PREFIX=${MAGNETAR_PREFIX:-$HOME/.local}
BIN_DIR=$PREFIX/bin
SHARE_DIR=$PREFIX/share/magnetar
CFG_DIR=${XDG_CONFIG_HOME:-$HOME/.config}/magnetar
ENV_FILE=${MAGNETAR_ENV_FILE:-$CFG_DIR/server.env}
DB_CONTAINER=${MAGNETAR_DB_CONTAINER:-magnetar-postgres}
DB_VOLUME=${MAGNETAR_DB_VOLUME:-magnetar-postgres-data}
DB_PORT=${MAGNETAR_DB_PORT:-54320}
PG_IMAGE=${MAGNETAR_PG_IMAGE:-postgres:18}

UPDATE=0
START=0
ACTION=install
for arg in "$@"; do
  case "$arg" in
    --update) UPDATE=1 ;;
    --start) START=1 ;;
    --print-token) ACTION=token ;;
    -h | --help) sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $arg (see --help)" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
rand_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

# Value of KEY in the env file, as sh sees it (a subshell: nothing leaks). The variable is
# unset first so a value from the caller's environment never stands in for a missing key.
env_value() { (set +u; unset "$1"; . "$ENV_FILE" >/dev/null 2>&1; eval "printf '%s' \"\${$1:-}\""); }

if [ "$ACTION" = token ]; then
  [ -f "$ENV_FILE" ] || die "$ENV_FILE not found"
  env_value MAGNETAR_API_TOKEN; echo; exit 0
fi

[ "$(id -u)" != 0 ] || [ -n "${MAGNETAR_ALLOW_ROOT:-}" ] ||
  die "run this as the user that will own the server (MAGNETAR_ALLOW_ROOT=1 to insist)"
[ "$(uname -s)" = Linux ] || die "this installer is for Linux; on macOS/Windows use Docker (docs/DOCKER.md)"

case "$(uname -m)" in
  x86_64 | amd64) ARCH=x86_64 ;;
  aarch64 | arm64) ARCH=aarch64 ;;
  *) ARCH="" ;;
esac
ASSET=magnetar-server-$ARCH-linux

work=$(mktemp -d "${TMPDIR:-/tmp}/magnetar-install.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

# --- 1. binary ---------------------------------------------------------------------------
step "1/4 magnetar-server -> $BIN_DIR"
mkdir -p "$BIN_DIR" "$SHARE_DIR"

fetch() { # fetch URL OUT
  if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die "neither curl nor wget is installed"
  fi
}

release_url() { # release_url ASSET
  if [ -n "${MAGNETAR_RELEASE_BASE:-}" ]; then
    echo "${MAGNETAR_RELEASE_BASE%/}/$1"
  elif [ -n "${MAGNETAR_VERSION:-}" ] && [ "$MAGNETAR_VERSION" != latest ]; then
    echo "https://github.com/$REPO/releases/download/$MAGNETAR_VERSION/$1"
  else
    echo "https://github.com/$REPO/releases/latest/download/$1"
  fi
}

sums_ok=0
fetch_sums() {
  [ "$sums_ok" = 1 ] && return 0
  fetch "$(release_url SHA256SUMS)" "$work/SHA256SUMS" ||
    die "cannot download SHA256SUMS of release ${MAGNETAR_VERSION:-latest} of $REPO (prerelease? set MAGNETAR_VERSION=<tag>)"
  sums_ok=1
}

# fetch_verified ASSET OUT: download one release asset and check it against SHA256SUMS.
fetch_verified() {
  fetch_sums
  fetch "$(release_url "$1")" "$2" || die "cannot download $1 of release ${MAGNETAR_VERSION:-latest} of $REPO"
  want=$(awk -v n="$1" '$2 == n || $2 == "*" n {print $1}' "$work/SHA256SUMS")
  [ -n "$want" ] || die "SHA256SUMS has no entry for $1"
  got=$(sha256sum "$2" | cut -d' ' -f1)
  [ "$want" = "$got" ] || die "checksum mismatch for $1 (want $want, got $got)"
  say "  $1: sha256 ok"
}

# install_file SRC DST: atomic replace (a running server keeps its old inode).
install_file() {
  cp "$1" "$2.new.$$"
  chmod 755 "$2.new.$$"
  mv -f "$2.new.$$" "$2"
}

if [ -n "${MAGNETAR_BINARY:-}" ]; then
  [ -f "$MAGNETAR_BINARY" ] || die "MAGNETAR_BINARY=$MAGNETAR_BINARY does not exist"
  cp "$MAGNETAR_BINARY" "$work/server"
  chmod 755 "$work/server"
  "$work/server" --version >/dev/null 2>&1 || die "$MAGNETAR_BINARY does not run on this host ($(uname -m))"
  install_file "$work/server" "$BIN_DIR/magnetar-server"
  say "  installed $MAGNETAR_BINARY"
elif [ -x "$BIN_DIR/magnetar-server" ] && [ "$UPDATE" = 0 ]; then
  say "  keeping installed binary (--update to fetch a new one)"
else
  [ -n "$ARCH" ] || die "no prebuilt binary for $(uname -m); build one (docs/LINUX.md) and set MAGNETAR_BINARY"
  fetch_verified "$ASSET" "$work/server"
  chmod 755 "$work/server"
  install_file "$work/server" "$BIN_DIR/magnetar-server"
fi
say "  $("$BIN_DIR/magnetar-server" --version)"

# magnetar-ctl and this installer: from the checkout this script runs in, else the release.
self_dir=""
case "$0" in
  */install-linux.sh | install-linux.sh) if [ -f "$0" ]; then self_dir=$(cd "$(dirname "$0")" && pwd); fi ;;
esac
for f in magnetar-ctl install-linux.sh; do
  case $f in magnetar-ctl) dst=$BIN_DIR/magnetar-ctl ;; *) dst=$SHARE_DIR/install-linux.sh ;; esac
  if [ -n "$self_dir" ] && [ -f "$self_dir/$f" ]; then
    [ "$self_dir/$f" -ef "$dst" ] || install_file "$self_dir/$f" "$dst"
  elif [ ! -f "$dst" ] || [ "$UPDATE" = 1 ]; then
    fetch_verified "$f" "$work/$f"
    install_file "$work/$f" "$dst"
  fi
done
say "  $BIN_DIR/magnetar-ctl"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR is not on PATH: add it, or call $BIN_DIR/magnetar-ctl" ;; esac

# --- 2. config ---------------------------------------------------------------------------
step "2/4 config $ENV_FILE"
mkdir -p "$(dirname "$ENV_FILE")"
chmod 700 "$(dirname "$ENV_FILE")"
umask 077
if [ ! -f "$ENV_FILE" ]; then
  printf '# Magnetar server config, written by install-linux.sh on %s.\n# Sourced by sh: keep values single-quoted. Edit, then: magnetar-ctl restart\n# All variables: docs/CONFIG.md\n' \
    "$(date -u +%Y-%m-%dT%H:%MZ)" >"$ENV_FILE"
fi
chmod 600 "$ENV_FILE"

has_key() { grep -q "^$1=" "$ENV_FILE"; }
# Values are single-quoted because the file is sourced by sh (URLs may contain '&').
put_key() {
  case $2 in *"'"*) die "$1 must not contain a single quote" ;; esac
  printf "%s='%s'\n" "$1" "$2" >>"$ENV_FILE"
}
ensure_key() { has_key "$1" || put_key "$1" "$2"; }
del_key() {
  has_key "$1" || return 0
  grep -v "^$1=" "$ENV_FILE" >"$work/env" || true
  cat "$work/env" >"$ENV_FILE"
}
# set_key: an explicit value from the environment replaces the file's one.
set_key() {
  if has_key "$1"; then
    [ "$(env_value "$1")" = "$2" ] && return 0
    del_key "$1"
    say "  updated $1"
  fi
  put_key "$1" "$2"
}

ensure_key MAGNETAR_API_TOKEN "$(rand_hex 32)"
ensure_key MAGNETAR_BIND "${MAGNETAR_BIND:-127.0.0.1:8787}"
ensure_key MAGNETAR_DOWNLOAD_DIR "${MAGNETAR_DOWNLOAD_DIR:-$HOME/Downloads/magnetar}"
ensure_key MAGNETAR_CRAWL_INTERVAL_SECS "${MAGNETAR_CRAWL_INTERVAL_SECS:-900}"
ensure_key MAGNETAR_TAILSCALE_SERVE "${MAGNETAR_TAILSCALE_SERVE:-0}"
ensure_key RUST_LOG "${RUST_LOG:-info}"
# server.log is a file: no ANSI colour codes in it (tracing-subscriber honours NO_COLOR).
ensure_key NO_COLOR "1"
[ -z "${MAGNETAR_ROOT_NPUB:-}" ] || set_key MAGNETAR_ROOT_NPUB "$MAGNETAR_ROOT_NPUB"
[ -z "${MAGNETAR_VERSION:-}" ] || set_key MAGNETAR_VERSION "$MAGNETAR_VERSION"
say "  ok (token kept if it existed)"

# --- 3. database -------------------------------------------------------------------------
step "3/4 Postgres"
docker_ok() { command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; }
db_mode=${MAGNETAR_DB:-}
if [ -z "$db_mode" ]; then
  if [ -n "${DATABASE_URL:-}" ]; then db_mode=url
  elif has_key DATABASE_URL && ! has_key MAGNETAR_DB_CONTAINER; then db_mode=url
  elif has_key MAGNETAR_DB_CONTAINER || docker_ok; then db_mode=docker
  else db_mode=manual
  fi
fi

case "$db_mode" in
  url)
    if [ -n "${DATABASE_URL:-}" ] && [ "$(env_value DATABASE_URL)" != "$DATABASE_URL" ]; then
      # A new external database: the docker container (if any) is no longer ours to start.
      del_key MAGNETAR_DB_CONTAINER
      set_key DATABASE_URL "$DATABASE_URL"
    fi
    has_key DATABASE_URL || die "MAGNETAR_DB=url but DATABASE_URL is not set"
    say "  using DATABASE_URL from $ENV_FILE"
    ;;
  docker)
    docker_ok || die "docker is not usable by $(id -un) (daemon running? in the docker group?)"
    if has_key MAGNETAR_DB_CONTAINER; then DB_CONTAINER=$(env_value MAGNETAR_DB_CONTAINER); fi
    ensure_key MAGNETAR_DB_PASSWORD "$(rand_hex 24)"
    pw=$(env_value MAGNETAR_DB_PASSWORD)
    if docker container inspect "$DB_CONTAINER" >/dev/null 2>&1; then
      say "  container $DB_CONTAINER exists: starting it"
      docker start "$DB_CONTAINER" >/dev/null
    else
      say "  creating container $DB_CONTAINER ($PG_IMAGE, volume $DB_VOLUME, 127.0.0.1:$DB_PORT)"
      docker run -d --name "$DB_CONTAINER" --restart unless-stopped \
        -e POSTGRES_USER=magnetar -e POSTGRES_PASSWORD="$pw" -e POSTGRES_DB=magnetar \
        -v "$DB_VOLUME:/var/lib/postgresql" -p "127.0.0.1:$DB_PORT:5432" \
        "$PG_IMAGE" >/dev/null
    fi
    i=0
    until docker exec "$DB_CONTAINER" pg_isready -q -U magnetar >/dev/null 2>&1; do
      i=$((i + 1)); [ "$i" -lt 90 ] || die "postgres in $DB_CONTAINER not ready after 90 s (docker logs $DB_CONTAINER)"
      sleep 1
    done
    ensure_key MAGNETAR_DB_CONTAINER "$DB_CONTAINER"
    ensure_key DATABASE_URL "postgres://magnetar:$pw@127.0.0.1:$DB_PORT/magnetar"
    say "  ready"
    ;;
  manual)
    cat <<EOF
  No DATABASE_URL and no usable docker. Install Postgres from your distro (18 is tested)
  (with contrib, for the trusted extension pg_trgm), then as the postgres superuser:

    sudo -u postgres createuser --pwprompt magnetar
    sudo -u postgres createdb -O magnetar magnetar

  and re-run with the URL (it is saved to $ENV_FILE):

    DATABASE_URL='postgres://magnetar:<password>@127.0.0.1:5432/magnetar' sh $0
EOF
    exit 1
    ;;
  *) die "MAGNETAR_DB must be docker, url or manual" ;;
esac

# --- 4. done -----------------------------------------------------------------------------
step "4/4 done"
if [ "$START" = 1 ]; then
  "$BIN_DIR/magnetar-ctl" up
fi
bind=$(env_value MAGNETAR_BIND)
cat <<EOF

  binary    $BIN_DIR/magnetar-server
  control   $BIN_DIR/magnetar-ctl up | down | restart | status | logs | token | update
  config    $ENV_FILE
  API token $(env_value MAGNETAR_API_TOKEN)
  listens   $bind

  Start it with: magnetar-ctl up   (again after a reboot; nothing is installed to autostart)
  In the Android app: Settings > Server URL = the address the phone reaches this host by,
  e.g. https://<host>.<tailnet>.ts.net with MAGNETAR_TAILSCALE_SERVE=1, and the token above.
EOF
