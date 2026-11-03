#!/usr/bin/env bash
# Local web UI check setup (docs/WEB.md "Local check", ARCHITECTURE.md D132–D135): a seeded
# scratch database and a server on 127.0.0.1:18787 with everything that talks to the network
# turned off.
#
#   scripts/webcheck.sh <state-dir> [seed|serve|both|stop|audit]
#
#   seed   recreate the scratch database magnetar_webcheck (webcheck_seed example) and write
#          <state-dir>/fixtures.json
#   serve  build, then run the server in the foreground on 127.0.0.1:18787 (Ctrl-C stops it;
#          its pid goes to <state-dir>/server.pid)
#   both   seed, then serve (default)
#   stop   stop the server started by serve (SIGINT to the pid in <state-dir>/server.pid,
#          only if that process is this checkout's magnetar-server)
#   audit  list the server's sockets: every peer must be 127.0.0.1 or ::1
#
# Needs the Postgres test container on 127.0.0.1:54329 (user/password magnetar). Never touches
# the production server or its database. The API token and the worker tokens are random,
# written once to <state-dir>/token, worker-eee, worker-rpi (mode 600), and never printed.
#
# WEBCHECK_NTFY=1 adds the ntfy variant (MAGNETAR_NTFY_URL=http://127.0.0.1:9, topic
# "webcheck"): the Settings and Saved pages then say alerts go out; anything the notifier
# would send goes to a closed loopback port.
set -euo pipefail

usage() { sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; }

[ $# -ge 1 ] || { usage >&2; exit 2; }
case "$1" in -h | --help) usage; exit 0 ;; esac
S=$1
mode=${2:-both}
root=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"
port=18787
admin=postgres://magnetar:magnetar@127.0.0.1:54329/magnetar
db=magnetar_webcheck
bin="${CARGO_TARGET_DIR:-$root/target}/debug/magnetar-server"

umask 077
mkdir -p "$S"/state "$S"/cache
S=$(cd "$S" && pwd)

secret() {
  # 32 random bytes as hex; created once, never echoed.
  [ -s "$1" ] || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$1"
  chmod 600 "$1"
}
secret "$S/token"
secret "$S/worker-eee"
secret "$S/worker-rpi"

audit() {
  local bad
  echo "sockets of magnetar-server:"
  ss -tunap 2>/dev/null | grep magnetar-server || echo "  (none)"
  # Peer column (6th): anything not loopback is a failure.
  bad=$(ss -tunapH 2>/dev/null | grep magnetar-server \
    | awk '{print $6}' | grep -vE '^(127\.0\.0\.1|\[::1\]|\[::ffff:127\.0\.0\.1\]|0\.0\.0\.0|\*|\[::\]):' || true)
  if [ -n "$bad" ]; then
    echo "NOT LOOPBACK: $bad" >&2
    return 1
  fi
  echo "audit: loopback only"
}

seed() {
  cd "$root"
  cargo run --offline -q -p magnetar-server --example webcheck_seed -- \
    --admin-url "$admin" --db "$db" --reset --out "$S/fixtures.json"
}

serve() {
  if [ -n "$(ss -ltnH "sport = :$port" 2>/dev/null)" ]; then
    echo "port $port busy" >&2
    exit 1
  fi
  cd "$root"
  cargo build --offline -q -p magnetar-server
  local ntfy=()
  if [ "${WEBCHECK_NTFY:-0}" = 1 ]; then
    ntfy=(MAGNETAR_NTFY_URL=http://127.0.0.1:9 MAGNETAR_NTFY_TOPIC=webcheck)
  fi
  # Wait for /health in the background, then say so (the server keeps the foreground).
  (
    for _ in $(seq 600); do
      if curl -s --max-time 2 "http://127.0.0.1:$port/health" 2>/dev/null | grep -q '"db":true'; then
        echo "webcheck: server ready on http://127.0.0.1:$port/" >&2
        exit 0
      fi
      sleep 1
    done
    echo "webcheck: the server did not answer /health" >&2
  ) &
  echo $$ >"$S/server.pid"
  # env -i: nothing from the caller's shell (TMDB token, ntfy topic, DATABASE_URL) leaks in.
  # exec: this shell's pid becomes the server's (server.pid).
  exec env -i HOME="$HOME" PATH="$PATH" \
    DATABASE_URL="postgres://magnetar:magnetar@127.0.0.1:54329/$db" \
    MAGNETAR_BIND="127.0.0.1:$port" MAGNETAR_API_TOKEN="$(cat "$S/token")" MAGNETAR_WEB_UI=true \
    MAGNETAR_NOSTR_CRAWL=false MAGNETAR_NIP66=false MAGNETAR_DHT=off MAGNETAR_DHT_PROBE=off \
    MAGNETAR_LIVENESS_DHT=false MAGNETAR_LIVENESS_FALLBACK_TRACKERS=false MAGNETAR_DOWNLOADS=false MAGNETAR_IMDB=false MAGNETAR_ANIME_DB=false \
    MAGNETAR_GEOIP=off MAGNETAR_GEOIP_DOWNLOAD=false MAGNETAR_TMDB_PREFETCH_PER_HOUR=0 \
    MAGNETAR_SEARCH_INDEX=true MAGNETAR_SEARCH_INDEX_DIR="$S/state/search-index" \
    MAGNETAR_DATASET_DIR="$S/state/datasets" MAGNETAR_IMAGE_CACHE_DIR="$S/cache/images" \
    MAGNETAR_WORKER_TOKENS="eee:$(cat "$S/worker-eee"),rpi:$(cat "$S/worker-rpi")" \
    XDG_STATE_HOME="$S/state" XDG_CACHE_HOME="$S/cache" RUST_LOG=info NO_COLOR=1 \
    "${ntfy[@]}" \
    "$bin" serve
}

stop() {
  local pid
  pid=$(cat "$S/server.pid" 2>/dev/null || true)
  if [ -z "$pid" ] || [ "$(readlink "/proc/$pid/exe" 2>/dev/null || true)" != "$(readlink -f "$bin")" ]; then
    echo "webcheck: no running check server (server.pid)" >&2
    rm -f "$S/server.pid"
    return 0
  fi
  kill -INT "$pid"
  for _ in $(seq 30); do
    [ -d "/proc/$pid" ] || break
    sleep 1
  done
  rm -f "$S/server.pid"
  echo "webcheck: server stopped" >&2
}

case "$mode" in
  seed) seed ;;
  serve) serve ;;
  both) seed; serve ;;
  stop) stop ;;
  audit) audit ;;
  *) usage >&2; exit 2 ;;
esac
