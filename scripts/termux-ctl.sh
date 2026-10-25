#!/data/data/com.termux/files/usr/bin/bash
# magnetar-ctl: manage the Magnetar server in Termux (installed by termux-setup.sh).
#
#   magnetar-ctl start | stop | restart | status | logs [-f] | token | health | update
#
# Uses the runit services when termux-setup.sh installed them, else pg_ctl + nohup.
# `stop` with runit lasts until the next boot; `termux-setup.sh --uninstall-services`
# removes autostart for good.
set -euo pipefail

: "${PREFIX:?run inside Termux}"
MAG_HOME=${MAGNETAR_HOME:-$HOME/magnetar}
ENV_FILE=$HOME/.config/magnetar/env
BIN=$MAG_HOME/bin/magnetar-server
SETUP=$MAG_HOME/bin/magnetar-setup
PGDATA=$PREFIX/var/lib/postgresql
PG_SOCKDIR=$PREFIX/tmp
PG_PORT=5432
PIDFILE=$PREFIX/var/run/magnetar.pid
export TMPDIR=$PREFIX/tmp
export SVDIR=$PREFIX/var/service
export LOGDIR=$PREFIX/var/log
NOHUP_LOG=$LOGDIR/magnetar.log
PG_LOG=$LOGDIR/postgres.log

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  echo "usage: magnetar-ctl {start|stop|restart|status|logs [-f]|token|health|update}" >&2
  exit 2
}

[ -f "$ENV_FILE" ] || die "$ENV_FILE not found: run termux-setup.sh first"

# shellcheck source=/dev/null
env_value() { (set -a && . "$ENV_FILE" && eval "printf '%s\n' \"\${$1:-}\""); }

health_url() {
  local bind host
  bind=$(env_value MAGNETAR_BIND)
  bind=${bind:-127.0.0.1:8787}
  host=${bind%:*}
  [ "$host" = 0.0.0.0 ] && host=127.0.0.1
  echo "http://$host:${bind##*:}/health"
}

use_runit() { [ -f "$SVDIR/magnetar/run" ] && command -v sv >/dev/null 2>&1; }

ensure_service_daemon() {
  local pidf=$PREFIX/var/run/service-daemon.pid
  [ -f "$pidf" ] && kill -0 "$(cat "$pidf")" 2>/dev/null && return 0
  (service-daemon start >/dev/null 2>&1 &)
  for _ in $(seq 20); do
    [ -p "$SVDIR/magnetar/supervise/ok" ] && return 0
    sleep 0.5
  done
  die "service-daemon (runsvdir) did not start"
}

pg_ready() { pg_isready -q -h "$PG_SOCKDIR" -p "$PG_PORT" >/dev/null 2>&1; }

server_pid() {
  [ -f "$PIDFILE" ] || return 1
  local pid
  pid=$(cat "$PIDFILE")
  kill -0 "$pid" 2>/dev/null || return 1
  echo "$pid"
}

do_start() {
  if use_runit; then
    ensure_service_daemon
    sv-enable postgres >/dev/null
    sv-enable magnetar >/dev/null
    sv status postgres magnetar
    return
  fi
  pg_ctl -D "$PGDATA" status >/dev/null 2>&1 || pg_ctl -D "$PGDATA" -w -l "$PG_LOG" start >/dev/null
  for _ in $(seq 30); do pg_ready && break; sleep 1; done
  pg_ready || die "postgres did not start, see $PG_LOG"
  if pid=$(server_pid); then
    echo "magnetar already running (pid $pid)"
    return
  fi
  [ -x "$BIN" ] || die "$BIN missing: run termux-setup.sh"
  # shellcheck disable=SC2016 # expanded by the inner sh
  nohup sh -c 'set -a; . "$1"; set +a; cd "$2"; exec "$3" serve' sh "$ENV_FILE" "$MAG_HOME" "$BIN" \
    >>"$NOHUP_LOG" 2>&1 </dev/null &
  echo $! >"$PIDFILE"
  echo "magnetar started (pid $!, log $NOHUP_LOG)"
}

do_stop() {
  if use_runit; then
    sv down magnetar postgres || true
    sv status postgres magnetar || true
    return
  fi
  if pid=$(server_pid); then
    kill "$pid"
    for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$pid" 2>/dev/null || true
    echo "magnetar stopped"
  fi
  rm -f "$PIDFILE"
  if pg_ctl -D "$PGDATA" status >/dev/null 2>&1; then
    pg_ctl -D "$PGDATA" -w -m fast stop >/dev/null
    echo "postgres stopped"
  fi
}

do_status() {
  local rc=0 url
  if use_runit; then
    echo "mode: runit services"
    sv status postgres magnetar || rc=1
  else
    echo "mode: nohup"
    if pid=$(server_pid); then echo "magnetar: running (pid $pid)"; else echo "magnetar: stopped"; rc=1; fi
    if pg_ctl -D "$PGDATA" status >/dev/null 2>&1; then echo "postgres: running"; else echo "postgres: stopped"; rc=1; fi
  fi
  url=$(health_url)
  if body=$(curl -fsS --max-time 3 "$url" 2>/dev/null); then
    echo "health: $body"
  else
    echo "health: no answer from $url"
    rc=1
  fi
  return "$rc"
}

do_logs() {
  local follow=()
  [ "${1:-}" = -f ] && follow=(-f)
  if use_runit; then
    tail -n 100 "${follow[@]}" "$LOGDIR/sv/magnetar/current" "$LOGDIR/sv/postgres/current"
  else
    tail -n 100 "${follow[@]}" "$NOHUP_LOG" "$PG_LOG"
  fi
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
  start) do_start ;;
  stop) do_stop ;;
  restart)
    if use_runit; then
      ensure_service_daemon
      sv-enable postgres >/dev/null
      sv restart magnetar
    else
      if pid=$(server_pid); then kill "$pid"; for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done; fi
      rm -f "$PIDFILE"
      do_start
    fi
    ;;
  status) do_status ;;
  logs) do_logs "$@" ;;
  token) env_value MAGNETAR_API_TOKEN ;;
  health) curl -fsS --max-time 3 "$(health_url)" && echo ;;
  update)
    [ -f "$SETUP" ] || die "$SETUP missing: re-run termux-setup.sh"
    if use_runit; then exec bash "$SETUP" --update "$@"; else exec bash "$SETUP" --update --no-services "$@"; fi
    ;;
  *) usage ;;
esac
