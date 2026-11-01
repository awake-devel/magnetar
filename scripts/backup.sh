#!/bin/sh
# backup.sh: dump the compose stack's database (pg_dump -Fc) to a local directory or to
# another host over ssh, and keep the newest N dumps there.
#
#   scripts/backup.sh                  dump to $MAGNETAR_BACKUP_DEST
#
# Settings (environment):
#   MAGNETAR_BACKUP_DEST  a local directory, or host:directory (ssh). Required.
#   MAGNETAR_BACKUP_KEEP  dumps to keep at the destination (default 7)
#   DOCKER                docker command (default docker; e.g. "sudo -n docker")
#   COMPOSE_DIR           checkout holding compose.yml (default: this script's parent)
#
# The dump is consistent while the server runs (one snapshot). It is written to a .part file
# and renamed only when pg_dump succeeded, so a failed run never replaces a good dump.
# Restore: docs/DOCKER.md "Backups".
set -eu

die() { printf 'backup: %s\n' "$*" >&2; exit 1; }

DEST=${MAGNETAR_BACKUP_DEST:?MAGNETAR_BACKUP_DEST is not set (a directory or host:directory)}
KEEP=${MAGNETAR_BACKUP_KEEP:-7}
DOCKER=${DOCKER:-docker}
COMPOSE_DIR=${COMPOSE_DIR:-$(cd "$(dirname "$0")/.." && pwd)}
case $KEEP in '' | *[!0-9]*) die "MAGNETAR_BACKUP_KEEP must be a number" ;; esac
[ "$KEEP" -ge 1 ] || die "MAGNETAR_BACKUP_KEEP must be at least 1"

name=magnetar-$(date +%Y-%m-%dT%H%M).dump
cd "$COMPOSE_DIR"
# shellcheck disable=SC2086 # DOCKER may be "sudo -n docker"
dump() { $DOCKER compose exec -T db sh -c 'pg_dump -U "$POSTGRES_USER" -Fc "$POSTGRES_DB"'; }

case $DEST in
  *:*)
    host=${DEST%%:*}
    dir=${DEST#*:}
    ssh -o BatchMode=yes "$host" "mkdir -p '$dir'" || die "cannot reach $host"
    # pipefail is not POSIX: check pg_dump's status through a marker file.
    status=$(mktemp)
    { dump || echo fail >"$status"; } | ssh -o BatchMode=yes "$host" "cat > '$dir/$name.part'"
    if [ -s "$status" ]; then
      rm -f "$status"
      ssh -o BatchMode=yes "$host" "rm -f '$dir/$name.part'"
      die "pg_dump failed; previous dumps kept"
    fi
    rm -f "$status"
    ssh -o BatchMode=yes "$host" "cd '$dir' && mv '$name.part' '$name' \
      && ls -1t magnetar-*.dump | tail -n +$((KEEP + 1)) | xargs -r rm -f --"
    size=$(ssh -o BatchMode=yes "$host" "du -h '$dir/$name' | cut -f1")
    ;;
  *)
    mkdir -p "$DEST"
    if ! dump >"$DEST/$name.part"; then
      rm -f "$DEST/$name.part"
      die "pg_dump failed; previous dumps kept"
    fi
    mv "$DEST/$name.part" "$DEST/$name"
    (cd "$DEST" && ls -1t magnetar-*.dump | tail -n +$((KEEP + 1)) | xargs -r rm -f --)
    size=$(du -h "$DEST/$name" | cut -f1)
    ;;
esac
printf 'backup: %s (%s) at %s, keeping %s\n' "$name" "$size" "$DEST" "$KEEP"
