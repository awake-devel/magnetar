#!/usr/bin/env bash
# shellcheck disable=SC2016 # check commands are single-quoted on purpose: they expand in the container
# Run scripts/termux-setup.sh for real inside termux/termux-docker:x86_64 and check the result.
#
#   scripts/termux-docker-test.sh [--mode services|nohup|both] [--binary PATH] [--keep]
#
# --binary  an x86_64-linux-android magnetar-server (default: the fake shim in
#           scripts/termux-docker-test/, which needs socat inside the container).
# --keep    leave the containers running for inspection (docker exec -u system ...).
#
# Docker cannot provide /dev/ashmem, so Termux's libandroid-shmem fails and Postgres cannot
# start. The harness builds scripts/termux-docker-test/fakeshm.c with the NDK and preloads it;
# nothing of that ships to phones. Termux app features (am, wake lock, Termux:Boot) are absent.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
image=termux/termux-docker:x86_64
mode=both
binary=""
keep=0
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) mode=$2; shift ;;
    --binary) binary=$(realpath "$2"); shift ;;
    --keep) keep=1 ;;
    -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

ndk=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}
clang=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang
[ -x "$clang" ] || { echo "NDK clang not found at $clang (set ANDROID_NDK_HOME)" >&2; exit 1; }

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
"$clang" --target=x86_64-linux-android24 -shared -fPIC -O2 -Wall \
  -o "$out/libfakeshm.so" "$root/scripts/termux-docker-test/fakeshm.c"

PREFIX=/data/data/com.termux/files/usr
THOME=/data/data/com.termux/files/home
pass=0
fail=0
ok() { printf '  PASS %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  FAIL %s\n' "$*"; fail=$((fail + 1)); }

# tx NAME CMD: run CMD as the Termux user with the shm shim preloaded.
tx() {
  docker exec -u system -e LD_PRELOAD="$PREFIX/lib/libfakeshm.so" -e SVDIR="$PREFIX/var/service" \
    -w "$THOME" "$1" bash -c "$2"
}
check() { # check NAME DESCRIPTION CMD
  if tx "$1" "$3" >"$out/check.log" 2>&1; then ok "$2"; else bad "$2"; sed 's/^/       /' "$out/check.log"; fi
}

run_mode() {
  local m=$1 name=magnetar-termux-test-$1 flag="" bin_in
  [ "$m" = nohup ] && flag=--no-services
  echo "=== mode $m (container $name)"
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" "$image" sleep infinity >/dev/null
  for _ in $(seq 60); do docker exec -u system "$name" test -x "$PREFIX/bin/pkg" 2>/dev/null && break; sleep 1; done

  # pkg picks a random mirror per run and some return 404; pin the default (Cloudflare) one.
  docker exec -u system "$name" ln -sf "$PREFIX/etc/termux/mirrors/default" "$PREFIX/etc/termux/chosen_mirrors"
  docker exec "$name" mkdir -p "$THOME/src/scripts"
  docker cp "$root/scripts/termux-setup.sh" "$name:$THOME/src/scripts/"
  docker cp "$root/scripts/termux-ctl.sh" "$name:$THOME/src/scripts/"
  docker cp "$out/libfakeshm.so" "$name:$PREFIX/lib/libfakeshm.so"
  if [ -n "$binary" ]; then
    docker cp "$binary" "$name:$THOME/src/magnetar-server"
  else
    docker cp "$root/scripts/termux-docker-test/fake-magnetar-server" "$name:$THOME/src/magnetar-server"
  fi
  docker exec "$name" chown -R system:system "$THOME/src" "$PREFIX/lib/libfakeshm.so"
  bin_in=$THOME/src/magnetar-server
  if [ -z "$binary" ]; then
    tx "$name" "DEBIAN_FRONTEND=noninteractive pkg install -y socat >/dev/null 2>&1" || { bad "install socat"; return; }
  fi

  local setup="MAGNETAR_BINARY=$bin_in bash src/scripts/termux-setup.sh $flag"
  echo "--- run 1"
  if tx "$name" "$setup" >"$out/run1.log" 2>&1; then ok "first run exits 0"; else bad "first run"; tail -n 40 "$out/run1.log"; return; fi
  sed 's/^/     | /' "$out/run1.log" | grep -E '==>|PASS|ERROR|!|API token|extensions' || true
  local env1 tok1
  env1=$(tx "$name" 'sha256sum ~/.config/magnetar/env')
  tok1=$(tx "$name" 'bash src/scripts/termux-setup.sh --print-token')

  check "$name" "env file is mode 600" '[ "$(stat -c %a ~/.config/magnetar/env)" = 600 ]'
  check "$name" "env file sources cleanly in sh" 'sh -c "set -a; . ~/.config/magnetar/env; [ -n \"\$DATABASE_URL\" ] && [ -n \"\$MAGNETAR_API_TOKEN\" ]"'
  check "$name" "PG 18 cluster at \$PREFIX/var/lib/postgresql" '[ "$(cat $PREFIX/var/lib/postgresql/PG_VERSION)" = 18 ]'
  check "$name" "socket in \$PREFIX/tmp" '[ -S $PREFIX/tmp/.s.PGSQL.5432 ]'
  check "$name" "DATABASE_URL (socket form) reaches db as role magnetar" \
    'set -a; . ~/.config/magnetar/env; [ "$(psql "$DATABASE_URL" -X -tAc "select current_user||'"'"'@'"'"'||current_database()")" = magnetar@magnetar ]'
  check "$name" "pg_trgm + unaccent installed" \
    '[ "$(psql -X -h $PREFIX/tmp -d magnetar -tAc "select count(*) from pg_extension where extname in ('"'pg_trgm','unaccent'"')")" = 2 ]'
  check "$name" "pg_trgm works (similarity)" \
    'psql -X -h $PREFIX/tmp -d magnetar -tAc "select similarity('"'magnetar','magnetor'"') > 0.3" | grep -qx t'
  check "$name" "settings applied (listen 127.0.0.1, max_connections 20, io_workers 1)" \
    '[ "$(psql -X -h $PREFIX/tmp -d postgres -tAc "select string_agg(setting, '"','"' order by name) from pg_settings where name in ('"'listen_addresses','max_connections','io_workers'"')")" = "1,127.0.0.1,20" ]'
  check "$name" "TCP 127.0.0.1 with password (scram) works" \
    'set -a; . ~/.config/magnetar/env; PGPASSWORD=$MAGNETAR_DB_PASSWORD psql -X -w -h 127.0.0.1 -U magnetar -d magnetar -tAc "select 1" | grep -qx 1'
  check "$name" "TCP 127.0.0.1 without password is rejected" \
    '! PGPASSWORD= psql -X -w -h 127.0.0.1 -U magnetar -d magnetar -tAc "select 1"'
  if [ -z "$binary" ]; then
    check "$name" "fake migrate ran" 'psql -X -h $PREFIX/tmp -d magnetar -tAc "select count(*) > 0 from fake_migrations" | grep -qx t'
  else
    check "$name" "migrations created tables" '[ "$(psql -X -h $PREFIX/tmp -d magnetar -tAc "select count(*) from pg_tables where schemaname = '"'public'"'")" -gt 0 ]'
  fi
  check "$name" "GET /health = 200 without token" 'curl -fsS http://127.0.0.1:8787/health'
  check "$name" "API without token = 401" '[ "$(curl -s -o /dev/null -w %{http_code} http://127.0.0.1:8787/api/v1/stats)" = 401 ]'
  if [ -n "$binary" ]; then
    check "$name" "API with token = 200" '[ "$(curl -s -o /dev/null -w %{http_code} -H "Authorization: Bearer $(magnetar-ctl token)" http://127.0.0.1:8787/api/v1/stats)" = 200 ]'
  fi
  check "$name" "magnetar-ctl on PATH, token matches" '[ "$(magnetar-ctl token)" = "'"$tok1"'" ]'
  check "$name" "magnetar-ctl status" 'magnetar-ctl status'
  check "$name" "boot script written" '[ -x ~/.termux/boot/start-magnetar ]'
  if [ "$m" = services ]; then
    check "$name" "runit: postgres + magnetar up" 'sv status postgres magnetar | grep -c "^run:" | grep -qx 2'
    check "$name" "runit log dir for magnetar" '[ -f $PREFIX/var/log/sv/magnetar/current ]'
  fi

  echo "--- run 2 (idempotency)"
  if tx "$name" "$setup" >"$out/run2.log" 2>&1; then ok "second run exits 0"; else bad "second run"; tail -n 40 "$out/run2.log"; fi
  expect() { if "${@:2}"; then ok "$1"; else bad "$1"; fi; }
  expect "env file unchanged" [ "$(tx "$name" 'sha256sum ~/.config/magnetar/env')" = "$env1" ]
  expect "token unchanged" [ "$(tx "$name" 'magnetar-ctl token')" = "$tok1" ]
  expect "no package reinstall" grep -q "already installed" "$out/run2.log"
  expect "no second initdb" grep -q "exists (PostgreSQL 18)" "$out/run2.log"
  check "$name" "single magnetar block in postgresql.conf" '[ "$(grep -c "^# >>> magnetar" $PREFIX/var/lib/postgresql/postgresql.conf)" = 1 ]'
  check "$name" "healthy after second run" 'curl -fsS http://127.0.0.1:8787/health'

  echo "--- magnetar-ctl stop/start/restart"
  check "$name" "stop" 'magnetar-ctl stop'
  check "$name" "health down after stop" '! curl -fsS --max-time 2 http://127.0.0.1:8787/health'
  check "$name" "start" 'magnetar-ctl start && for i in $(seq 30); do curl -fsS http://127.0.0.1:8787/health && exit 0; sleep 1; done; exit 1'
  check "$name" "restart" 'magnetar-ctl restart && for i in $(seq 30); do curl -fsS http://127.0.0.1:8787/health && exit 0; sleep 1; done; exit 1'
  check "$name" "update (re-installs binary, migrates, restarts)" "MAGNETAR_BINARY=$bin_in magnetar-ctl update"

  if [ "$m" = services ]; then
    echo "--- --uninstall-services"
    check "$name" "uninstall services" 'bash src/scripts/termux-setup.sh --uninstall-services && [ ! -d $PREFIX/var/service/magnetar ] && [ -f $PREFIX/var/service/postgres/down ] && [ ! -e ~/.termux/boot/start-magnetar ]'
    check "$name" "data kept after uninstall" '[ -f $PREFIX/var/lib/postgresql/PG_VERSION ] && [ -f ~/.config/magnetar/env ]'
  fi

  if [ "$keep" = 0 ]; then docker rm -f "$name" >/dev/null; else echo "kept container $name"; fi
}

case "$mode" in
  both) run_mode services; run_mode nohup ;;
  services | nohup) run_mode "$mode" ;;
  *) echo "--mode services|nohup|both" >&2; exit 2 ;;
esac
echo
echo "termux-docker test: $pass passed, $fail failed"
[ "$fail" = 0 ]
