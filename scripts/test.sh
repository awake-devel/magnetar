#!/usr/bin/env bash
# The "done" gate: fmt, clippy -D warnings, workspace tests against Postgres 18, Android build.
#
#   scripts/test.sh [--ignored] [--termux] [--no-android] [-- <extra cargo test args>]
#
# --ignored     also run the #[ignore] network tests (real relays/trackers) afterwards
# --termux      also run scripts/termux-docker-test.sh (termux-setup.sh inside termux-docker)
# --no-android  skip scripts/build-android.sh
#
# MAGNETAR_TEST_DATABASE_URL is used as given; when unset, the docker container magnetar-pg
# (postgres:18 on 127.0.0.1:54329) is started or created and its URL exported.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}

ignored=0
termux=0
android=1
extra=()
while [ $# -gt 0 ]; do
  case "$1" in
    --ignored) ignored=1 ;;
    --termux) termux=1 ;;
    --no-android) android=0 ;;
    --) shift; extra=("$@"); break ;;
    -h | --help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n==> %s\n' "$*"; }
started=$(date +%s)

ensure_postgres() {
  if [ -n "${MAGNETAR_TEST_DATABASE_URL:-}" ]; then
    echo "using MAGNETAR_TEST_DATABASE_URL from the environment"
    return
  fi
  local name=magnetar-pg
  if [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" = true ]; then
    echo "$name already running"
  elif docker inspect "$name" >/dev/null 2>&1; then
    docker start "$name" >/dev/null
    echo "started $name"
  else
    docker run -d --name "$name" -p 127.0.0.1:54329:5432 \
      -e POSTGRES_USER=magnetar -e POSTGRES_PASSWORD=magnetar -e POSTGRES_DB=magnetar \
      postgres:18 >/dev/null
    echo "created $name (postgres:18 on 127.0.0.1:54329)"
  fi
  for _ in $(seq 60); do
    docker exec "$name" pg_isready -q -U magnetar -d magnetar && break
    sleep 1
  done
  docker exec "$name" pg_isready -q -U magnetar -d magnetar || { echo "postgres did not become ready" >&2; exit 1; }
  export MAGNETAR_TEST_DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/magnetar
}

cd "$root"
step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy --workspace --all-targets --all-features -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings

step "Postgres for tests"
ensure_postgres

step "cargo test --workspace --all-features"
cargo test --workspace --all-features -- "${extra[@]}"

if [ "$ignored" = 1 ]; then
  step "cargo test -- --ignored (network tests: real relays and trackers)"
  cargo test --workspace --all-features -- --ignored "${extra[@]}"
fi

if [ "$termux" = 1 ]; then
  step "termux-docker test of termux-setup.sh"
  bash "$root/scripts/termux-docker-test.sh"
fi

if [ "$android" = 1 ]; then
  step "Android build"
  if [ -x "$root/scripts/build-android.sh" ] || [ -f "$root/scripts/build-android.sh" ]; then
    bash "$root/scripts/build-android.sh"
  else
    echo "skipped: scripts/build-android.sh does not exist yet"
  fi
fi

printf '\nALL GREEN in %ss\n' "$(($(date +%s) - started))"
