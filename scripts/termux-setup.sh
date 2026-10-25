#!/data/data/com.termux/files/usr/bin/bash
# Magnetar server setup for Termux. Idempotent: re-running it never regenerates secrets,
# never re-initialises the database and only rewrites files it owns.
#
#   bash termux-setup.sh                  full install with runit services (termux-services)
#   bash termux-setup.sh --no-services    same, but run postgres + server with pg_ctl/nohup
#   bash termux-setup.sh --update         re-fetch the server binary, migrate, restart
#   bash termux-setup.sh --uninstall-services
#   bash termux-setup.sh --print-token
#
# Binary source (first match wins):
#   MAGNETAR_BINARY=/path/to/magnetar-server   install a local file
#   MAGNETAR_BUILD=local                       build from source with `pkg install rust`
#   default                                    download the GitHub release asset
#     MAGNETAR_VERSION=v0.1.0   pick a tag instead of "latest" (needed for prereleases)
#     GITHUB_TOKEN=...          required while the repository is private
#
# Other knobs: MAGNETAR_HOME (default ~/magnetar), MAGNETAR_REPO (owner/name),
# MAGNETAR_SRC (source checkout for MAGNETAR_BUILD=local), MAGNETAR_ROOT_NPUB,
# MAGNETAR_SKIP_HEALTH=1. See docs/TERMUX.md.
set -euo pipefail

if [ -z "${PREFIX:-}" ] || ! command -v pkg >/dev/null 2>&1; then
  echo "termux-setup.sh must run inside Termux (PREFIX/pkg not found)" >&2
  exit 1
fi
if [ "$(id -u)" = 0 ]; then
  echo "do not run this as root: Termux packages and services belong to the app user" >&2
  exit 1
fi

REPO=${MAGNETAR_REPO:-awake-devel/magnetar}
MAG_HOME=${MAGNETAR_HOME:-$HOME/magnetar}
CFG_DIR=$HOME/.config/magnetar
ENV_FILE=$CFG_DIR/env
BIN=$MAG_HOME/bin/magnetar-server
CTL=$MAG_HOME/bin/magnetar-ctl
SELF_COPY=$MAG_HOME/bin/magnetar-setup
PGDATA=$PREFIX/var/lib/postgresql # the only path the stock runit service uses
PG_SOCKDIR=$PREFIX/tmp
PG_PORT=5432
PIDFILE=$PREFIX/var/run/magnetar.pid
BOOT_SCRIPT=$HOME/.termux/boot/start-magnetar

export TMPDIR=$PREFIX/tmp
export SVDIR=$PREFIX/var/service
export LOGDIR=$PREFIX/var/log

MODE=services
UPDATE=0
ACTION=install

usage() { sed -n '2,22p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --no-services) MODE="nohup" ;;
    --update) UPDATE=1 ;;
    --uninstall-services) ACTION=uninstall ;;
    --print-token) ACTION=token ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '  ! %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

rand_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

load_env() {
  set -a
  # shellcheck source=/dev/null
  . "$ENV_FILE"
  set +a
}

print_token() {
  [ -f "$ENV_FILE" ] || die "$ENV_FILE not found: run the setup first"
  (load_env && printf '%s\n' "$MAGNETAR_API_TOKEN")
}

pg() { pg_ctl -D "$PGDATA" "$@"; }
pg_ready() { pg_isready -q -h "$PG_SOCKDIR" -p "$PG_PORT" >/dev/null 2>&1; }
wait_pg() {
  for _ in $(seq 30); do pg_ready && return 0; sleep 1; done
  return 1
}

daemon_running() {
  local pidf=$PREFIX/var/run/service-daemon.pid
  [ -f "$pidf" ] && kill -0 "$(cat "$pidf")" 2>/dev/null
}

start_service_daemon() {
  daemon_running && return 0
  # What $PREFIX/etc/profile.d/start-services.sh does in a fresh session.
  (service-daemon start >/dev/null 2>&1 &)
  for _ in $(seq 20); do
    [ -p "$SVDIR/postgres/supervise/ok" ] && return 0
    sleep 0.5
  done
  die "runit (service-daemon) did not start; try --no-services"
}

sv_up() { sv status "$1" 2>/dev/null | grep -q '^run:'; }

nohup_server_running() { [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; }

stop_nohup_server() {
  nohup_server_running || { rm -f "$PIDFILE"; return 0; }
  local pid
  pid=$(cat "$PIDFILE")
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$PIDFILE"
}

uninstall_services() {
  if [ -d "$SVDIR/magnetar" ]; then
    sv down magnetar >/dev/null 2>&1 || true
    rm -rf "$SVDIR/magnetar"
    info "removed runit service magnetar"
  fi
  if [ -d "$SVDIR/postgres" ] && [ ! -f "$SVDIR/postgres/down" ]; then
    sv down postgres >/dev/null 2>&1 || true
    touch "$SVDIR/postgres/down"
    info "disabled runit service postgres (data kept in $PGDATA)"
  fi
  if [ -f "$BOOT_SCRIPT" ]; then
    rm -f "$BOOT_SCRIPT"
    info "removed $BOOT_SCRIPT"
  fi
}

case "$ACTION" in
  token) print_token; exit 0 ;;
  uninstall)
    step "Removing Magnetar services (binary, config and database are kept)"
    uninstall_services
    info "start manually with: $CTL start   (nohup mode)"
    info "to delete everything: rm -rf $MAG_HOME $CFG_DIR $PGDATA"
    exit 0
    ;;
esac

# --- 1. packages -------------------------------------------------------------------------
step "1/9 Termux packages"
want=(postgresql curl jq)
[ "$MODE" = services ] && want+=(termux-services)
binary_mode=${MAGNETAR_BINARY:+file}
binary_mode=${binary_mode:-${MAGNETAR_BUILD:-release}}
[ "$binary_mode" = local ] && want+=(rust git cmake)
missing=()
for p in "${want[@]}"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
if [ ${#missing[@]} -gt 0 ]; then
  info "installing: ${missing[*]}"
  DEBIAN_FRONTEND=noninteractive pkg install -y "${missing[@]}" ||
    die "pkg install failed. A broken mirror? Run termux-change-repo, then re-run this script"
else
  info "already installed: ${want[*]}"
fi

# --- 2. config + secrets -----------------------------------------------------------------
step "2/9 Config $ENV_FILE"
mkdir -p "$CFG_DIR" "$MAG_HOME/bin" "$PREFIX/var/run" "$LOGDIR"
chmod 700 "$CFG_DIR"
created_env=0
[ -f "$ENV_FILE" ] || created_env=1
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"

if [ -d "$HOME/storage/downloads" ]; then
  default_dl=$HOME/storage/downloads/magnetar
else
  default_dl=$MAG_HOME/downloads # run termux-setup-storage to use shared storage instead
fi

# Values are single-quoted because the file is sourced by sh (DATABASE_URL contains '&').
ensure_key() {
  grep -q "^$1=" "$ENV_FILE" || printf "%s='%s'\n" "$1" "$2" >>"$ENV_FILE"
}
[ "$created_env" = 1 ] && printf '# Magnetar server config, generated by termux-setup.sh on %s.\n# Sourced by sh: keep values single-quoted. Edit, then: magnetar-ctl restart\n' \
  "$(date -u +%Y-%m-%dT%H:%MZ)" >>"$ENV_FILE"
ensure_key DATABASE_URL "postgres:///magnetar?host=$PG_SOCKDIR&user=magnetar"
ensure_key MAGNETAR_DB_PASSWORD "$(rand_hex 24)"
ensure_key MAGNETAR_BIND "127.0.0.1:8787"
ensure_key MAGNETAR_API_TOKEN "$(rand_hex 32)"
ensure_key MAGNETAR_NTFY_URL "https://ntfy.sh"
ensure_key MAGNETAR_NTFY_TOPIC "magnetar-$(rand_hex 12)"
ensure_key MAGNETAR_DOWNLOAD_DIR "$default_dl"
ensure_key MAGNETAR_CRAWL_INTERVAL_SECS "900"
ensure_key RUST_LOG "info"
ensure_key TMPDIR "$PREFIX/tmp"
if [ -n "${MAGNETAR_ROOT_NPUB:-}" ]; then
  ensure_key MAGNETAR_ROOT_NPUB "$MAGNETAR_ROOT_NPUB"
elif ! grep -q 'MAGNETAR_ROOT_NPUB=' "$ENV_FILE"; then
  echo "# MAGNETAR_ROOT_NPUB='npub1...'   # your npub: root of the web of trust" >>"$ENV_FILE"
fi
grep -q 'MAGNETAR_SEED_RELAYS=' "$ENV_FILE" ||
  echo "# MAGNETAR_SEED_RELAYS='wss://nos.lol,wss://relay.primal.net'   # unset = built-in list" >>"$ENV_FILE"
load_env
mkdir -p "$MAGNETAR_DOWNLOAD_DIR"
info "$([ "$created_env" = 1 ] && echo created || echo kept) (secrets are never regenerated)"

# --- 3. postgres cluster -----------------------------------------------------------------
step "3/9 PostgreSQL cluster $PGDATA"
PG_SUPERUSER=$(whoami)
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  mkdir -p "$PGDATA"
  initdb -D "$PGDATA" -U "$PG_SUPERUSER" -E UTF8 --locale=C.UTF-8 -A trust >"$LOGDIR/magnetar-initdb.log" 2>&1 ||
    die "initdb failed, see $LOGDIR/magnetar-initdb.log"
  info "initialised ($(cat "$PGDATA/PG_VERSION"))"
else
  info "exists (PostgreSQL $(cat "$PGDATA/PG_VERSION"))"
fi

conf=$PGDATA/postgresql.conf
hba=$PGDATA/pg_hba.conf
before=$(cat "$conf" "$hba" | cksum)
sed -i '/^# >>> magnetar/,/^# <<< magnetar/d' "$conf"
cat >>"$conf" <<EOF
# >>> magnetar (managed by termux-setup.sh; edits inside this block are overwritten)
listen_addresses = '127.0.0.1'
port = $PG_PORT
unix_socket_directories = '$PG_SOCKDIR'
max_connections = 20      # every backend is a process: Android phantom-process budget
shared_buffers = 64MB
io_workers = 1            # PG 18 default is 3 worker processes
# <<< magnetar
EOF
cat >"$hba" <<'EOF'
# managed by termux-setup.sh
# The socket lives in Termux's private app dir; 127.0.0.1 is shared by every app on the phone.
local   all  all                 trust
host    all  all  127.0.0.1/32   scram-sha-256
EOF
conf_changed=0
[ "$before" = "$(cat "$conf" "$hba" | cksum)" ] || conf_changed=1

# --- 4. role, database, extensions ------------------------------------------------------
step "4/9 Database role and extensions"
pg_by_runit=0
pg_started_here=0
if [ -d "$SVDIR/postgres/supervise" ] && sv_up postgres; then
  pg_by_runit=1
  [ "$conf_changed" = 1 ] && { info "config changed: restarting runit postgres"; sv restart postgres >/dev/null; }
elif pg status >/dev/null 2>&1; then
  [ "$conf_changed" = 1 ] && { info "config changed: restarting postgres"; pg -w -m fast restart -l "$LOGDIR/postgres.log" >/dev/null; }
else
  pg -w -l "$LOGDIR/postgres.log" start >/dev/null
  pg_started_here=1
fi
# Never leave a pg_ctl-managed cluster behind that would fight with runit after a failure.
cleanup() {
  if [ "$pg_started_here" = 1 ] && [ "$MODE" = services ]; then pg -w -m fast stop >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
wait_pg || die "postgres is not accepting connections, see $LOGDIR/postgres.log"

psql_su() { psql -X -q -v ON_ERROR_STOP=1 -h "$PG_SOCKDIR" -p "$PG_PORT" -U "$PG_SUPERUSER" "$@"; }
# The password goes through a psql variable (stdin script), never into the SQL text or argv of the server.
psql_su -d postgres -v pw="$MAGNETAR_DB_PASSWORD" <<'SQL'
SELECT 'CREATE ROLE magnetar LOGIN' WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'magnetar') \gexec
SELECT format('ALTER ROLE magnetar LOGIN PASSWORD %L', :'pw') \gexec
SELECT 'CREATE DATABASE magnetar OWNER magnetar' WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'magnetar') \gexec
SQL
psql_su -d magnetar <<'SQL'
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS unaccent;
SQL
info "role magnetar, database magnetar, extensions: $(psql_su -d magnetar -tAc "select string_agg(extname || ' ' || extversion, ', ' order by extname) from pg_extension")"

# --- 5. server binary --------------------------------------------------------------------
step "5/9 Server binary $BIN"
case "$(uname -m)" in
  aarch64 | arm64) triple=aarch64-linux-android ;;
  x86_64) triple=x86_64-linux-android ;;
  *) triple="" ;;
esac
asset=magnetar-server-$triple

work=$(mktemp -d "$TMPDIR/magnetar-setup.XXXXXX")
trap 'rm -rf "$work"; cleanup' EXIT

gh_headers() {
  # Header file keeps the token out of the process list.
  printf 'Authorization: Bearer %s\nX-GitHub-Api-Version: 2022-11-28\n' "$GITHUB_TOKEN" >"$work/h"
  chmod 600 "$work/h"
}

# download_asset NAME OUT: fetch one asset of the selected release.
download_asset() {
  local name=$1 out=$2 tag=${MAGNETAR_VERSION:-}
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    if [ ! -f "$work/release.json" ]; then
      gh_headers
      local api=https://api.github.com/repos/$REPO/releases/latest
      [ -n "$tag" ] && api=https://api.github.com/repos/$REPO/releases/tags/$tag
      curl -fsSL -H @"$work/h" -H 'Accept: application/vnd.github+json' -o "$work/release.json" "$api" ||
        { warn "GitHub API: cannot read release ${tag:-latest} of $REPO (token scope? prerelease? set MAGNETAR_VERSION)"; return 1; }
    fi
    local id
    id=$(jq -r --arg n "$name" '.assets[] | select(.name == $n) | .id' "$work/release.json" | head -n1)
    [ -n "$id" ] || return 1
    # curl drops the Authorization header on the redirect to the storage host.
    curl -fsSL --retry 3 -H @"$work/h" -H 'Accept: application/octet-stream' -o "$out" \
      "https://api.github.com/repos/$REPO/releases/assets/$id"
  else
    local url=https://github.com/$REPO/releases/latest/download/$name
    [ -n "$tag" ] && url=https://github.com/$REPO/releases/download/$tag/$name
    curl -fsSL --retry 3 -o "$out" "$url"
  fi
}

fetch_release() {
  [ -n "$triple" ] || die "no prebuilt binary for $(uname -m); use MAGNETAR_BUILD=local"
  info "downloading $asset from $REPO (${MAGNETAR_VERSION:-latest})"
  download_asset "$asset" "$work/bin" ||
    die "download failed. Private repo? export GITHUB_TOKEN=<token with contents:read>. Or set MAGNETAR_BINARY=/path"
  if download_asset SHA256SUMS "$work/sums" 2>/dev/null; then
    local want_sum got_sum
    want_sum=$(awk -v n="$asset" '$2 == n || $2 == "*" n {print $1}' "$work/sums")
    got_sum=$(sha256sum "$work/bin" | cut -d' ' -f1)
    [ -n "$want_sum" ] || die "SHA256SUMS has no entry for $asset"
    [ "$want_sum" = "$got_sum" ] || die "checksum mismatch for $asset"
    info "sha256 ok"
  else
    warn "no SHA256SUMS in the release: checksum not verified"
  fi
}

build_local() {
  local src=${MAGNETAR_SRC:-$MAG_HOME/src}
  if [ ! -f "$src/Cargo.toml" ]; then
    info "cloning $REPO into $src"
    if [ -n "${GITHUB_TOKEN:-}" ]; then
      local basic
      basic=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')
      git -c "http.extraHeader=Authorization: Basic $basic" clone --depth 1 "https://github.com/$REPO" "$src"
    else
      git clone --depth 1 "https://github.com/$REPO" "$src"
    fi
  fi
  info "cargo build --release -p magnetar-server (slow on a phone)"
  (cd "$src" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build --release --locked -p magnetar-server)
  cp "$src/target/release/magnetar-server" "$work/bin"
}

binary_changed=0
if [ "$binary_mode" = file ]; then
  [ -f "$MAGNETAR_BINARY" ] || die "MAGNETAR_BINARY=$MAGNETAR_BINARY does not exist"
  cp "$MAGNETAR_BINARY" "$work/bin"
elif [ -x "$BIN" ] && [ "$UPDATE" = 0 ]; then
  info "keeping installed binary (use --update to fetch a new one)"
elif [ "$binary_mode" = local ]; then
  build_local
else
  fetch_release
fi
if [ -f "$work/bin" ]; then
  chmod 755 "$work/bin"
  "$work/bin" --version >/dev/null 2>&1 || die "the new binary does not run (wrong architecture or corrupt download)"
  if [ -f "$BIN" ] && cmp -s "$work/bin" "$BIN"; then
    info "binary unchanged"
  else
    mv -f "$work/bin" "$BIN"
    binary_changed=1
  fi
fi
info "$("$BIN" --version)"

# --- 6. migrations -----------------------------------------------------------------------
step "6/9 Database migrations"
"$BIN" migrate

# --- 7. helper scripts -------------------------------------------------------------------
step "7/9 Helper scripts"
here=""
[ -f "${BASH_SOURCE[0]:-}" ] && here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
install_helper() { # install_helper SOURCE_NAME DEST
  if [ -n "$here" ] && [ -f "$here/$1" ]; then
    [ "$here/$1" -ef "$2" ] || install -m 755 "$here/$1" "$2"
  elif [ "$binary_mode" = release ] && download_asset "$1" "$work/$1" 2>/dev/null; then
    install -m 755 "$work/$1" "$2"
  elif [ ! -x "$2" ]; then
    warn "could not find $1 next to this script or in the release"
    return 1
  fi
}
install_helper termux-ctl.sh "$CTL" && ln -sf "$CTL" "$PREFIX/bin/magnetar-ctl" && info "magnetar-ctl -> $CTL"
install_helper termux-setup.sh "$SELF_COPY" && info "setup copy -> $SELF_COPY (used by magnetar-ctl update)" || true

# --- 8. start ----------------------------------------------------------------------------
step "8/9 Start ($MODE mode)"
if [ "$MODE" = services ]; then
  stop_nohup_server
  # Hand the cluster over to runit: it must not also be managed by pg_ctl.
  if [ "$pg_by_runit" = 0 ]; then pg -w -m fast stop >/dev/null; fi
  pg_started_here=0
  if [ ! -d "$SVDIR/magnetar" ]; then
    mkdir -p "$SVDIR/magnetar/log"
    touch "$SVDIR/magnetar/down" # sv-enable below starts it, after postgres
  fi
  run_new=$(
    cat <<EOF
#!$PREFIX/bin/sh
# Generated by termux-setup.sh; re-run it to regenerate.
exec 2>&1
export TMPDIR='$PREFIX/tmp'
set -a
. '$ENV_FILE'
set +a
cd '$MAG_HOME'
# postgres is a sibling runit service: wait for its socket, runit retries if we give up.
i=0
while [ ! -S '$PG_SOCKDIR/.s.PGSQL.$PG_PORT' ]; do
  i=\$((i + 1)); [ \$i -gt 60 ] && { echo 'postgres socket not found after 60 s'; exit 1; }
  sleep 1
done
exec '$BIN' serve
EOF
  )
  run_changed=0
  if [ ! -f "$SVDIR/magnetar/run" ] || [ "$(cat "$SVDIR/magnetar/run")" != "$run_new" ]; then
    printf '%s\n' "$run_new" >"$SVDIR/magnetar/run"
    run_changed=1
  fi
  # svlogger's calling convention, copied verbatim from the packaged service.
  cp -f "$SVDIR/postgres/log/run" "$SVDIR/magnetar/log/run"
  chmod 755 "$SVDIR/magnetar/run" "$SVDIR/magnetar/log/run"
  start_service_daemon
  # runsvdir rescans every 5 s; wait until it supervises the new service.
  for _ in $(seq 20); do [ -p "$SVDIR/magnetar/supervise/ok" ] && break; sleep 0.5; done
  [ -p "$SVDIR/magnetar/supervise/ok" ] || die "runit did not pick up $SVDIR/magnetar"
  was_up=0
  sv_up magnetar && was_up=1
  sv-enable postgres >/dev/null
  wait_pg || die "runit postgres did not come up, see $LOGDIR/sv/postgres/current"
  sv-enable magnetar >/dev/null
  if [ "$was_up" = 1 ] && { [ "$binary_changed" = 1 ] || [ "$run_changed" = 1 ] || [ "$UPDATE" = 1 ]; }; then
    sv restart magnetar >/dev/null
    info "restarted magnetar"
  fi
  sv status postgres magnetar | sed 's/^/    /'
else
  if [ -d "$SVDIR/magnetar" ] || { [ -d "$SVDIR/postgres" ] && [ ! -f "$SVDIR/postgres/down" ]; }; then
    info "switching from runit to nohup mode"
    uninstall_services
  fi
  pg_started_here=0 # keep the pg_ctl cluster running: nohup mode uses it
  if [ "$binary_changed" = 1 ] || [ "$UPDATE" = 1 ]; then stop_nohup_server; fi
  "$CTL" start
fi

# --- 9. boot + wake lock -----------------------------------------------------------------
step "9/9 Termux:Boot script and wake lock"
mkdir -p "$(dirname "$BOOT_SCRIPT")"
if [ "$MODE" = services ]; then
  printf '#!%s/bin/sh\n# Generated by termux-setup.sh\ntermux-wake-lock\n. %s/etc/profile.d/start-services.sh\n' \
    "$PREFIX" "$PREFIX" >"$BOOT_SCRIPT"
else
  printf '#!%s/bin/sh\n# Generated by termux-setup.sh\nexport TMPDIR=%s/tmp\ntermux-wake-lock\nexec %s start\n' \
    "$PREFIX" "$PREFIX" "$CTL" >"$BOOT_SCRIPT"
fi
chmod 755 "$BOOT_SCRIPT"
info "$BOOT_SCRIPT (runs at boot once the Termux:Boot app has been opened one time)"
if command -v termux-wake-lock >/dev/null 2>&1 && termux-wake-lock >/dev/null 2>&1; then
  info "wake lock held (release with termux-wake-unlock)"
else
  warn "termux-wake-lock failed (is this a real Termux app?)"
fi

# --- health ------------------------------------------------------------------------------
bind_host=${MAGNETAR_BIND%:*}
bind_port=${MAGNETAR_BIND##*:}
[ "$bind_host" = 0.0.0.0 ] && bind_host=127.0.0.1
health_url=http://$bind_host:$bind_port/health
if [ "${MAGNETAR_SKIP_HEALTH:-0}" = 1 ]; then
  warn "health check skipped (MAGNETAR_SKIP_HEALTH=1)"
else
  step "Health check $health_url"
  ok=0
  for _ in $(seq 30); do
    if body=$(curl -fsS --max-time 2 "$health_url" 2>/dev/null); then ok=1; break; fi
    sleep 1
  done
  if [ "$ok" = 1 ]; then
    info "$body"
  else
    warn "no answer from $health_url after 30 s. Logs: magnetar-ctl logs"
    exit 1
  fi
fi

cat <<EOF

Magnetar is running.

  API token   $MAGNETAR_API_TOKEN
  Server URL  http://$bind_host:$bind_port
  ntfy topic  $MAGNETAR_NTFY_TOPIC   (subscribe in the ntfy app from F-Droid, server $MAGNETAR_NTFY_URL)

Next steps
  1. In the Magnetar app: Settings > Server URL = http://$bind_host:$bind_port, API token = the
     value above (long-press to copy it here; print it again with: magnetar-ctl token).
  2. Android 12+ kills Termux child processes (phantom process killer). Disable it once:
       Android 14+: Developer options > "Disable child process restrictions"
       Android 12L/13: adb shell settings put global settings_enable_monitor_phantom_procs false
  3. Exempt Termux from battery optimisation (Android settings > Apps > Termux > Battery).
  4. Install Termux:Boot from the same source as Termux (F-Droid) and open it once.
  Manage the server with: magnetar-ctl {start|stop|restart|status|logs|token|update}
EOF
