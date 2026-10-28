#!/usr/bin/env bash
# run.sh <label> <binary> <port-base> [extra env assignments...]
# One 600 s aggressive crawl against a fresh scratch DB; logs in ../logs/<label>.
set -u
label=$1; bin=$2; port=$3; shift 3
out=~/.cache/magnetar-balance-scratch/logs/$label
mkdir -p "$out"
src=$(ip route get 1.1.1.1 | grep -o 'src [0-9.]*' | cut -d' ' -f2)
[ "$src" = "<ip>" ] && echo "route check: direct fiber, ok" > "$out/route.txt"
if [ "$src" != "<ip>" ]; then echo "NOT on direct fiber, abort"; exit 3; fi
db=mag_bal_$label
PGU=postgres://magnetar:magnetar@127.0.0.1:54329
psql $PGU/postgres -qc "drop database if exists $db" -c "create database $db" || exit 4
export DATABASE_URL=$PGU/$db
export MAGNETAR_DHT_PORT=$port MAGNETAR_DHT_QPS=3000 MAGNETAR_DHT_PEER_LOOKUPS=2048 \
  MAGNETAR_DHT_FETCH_CONCURRENCY=2048 MAGNETAR_DHT_TCP_RATE=1000 MAGNETAR_DHT_TCP_MAX=4000 \
  MAGNETAR_DHT_UPNP=false RUST_LOG=info
for kv in "$@"; do export "$kv"; done
date -Is > "$out/start.txt"
"$bin" dht --mode aggressive --seconds 600 > "$out/report.json" 2> "$out/stderr.log" &
pid=$!
bad=0; t=0
: > "$out/samples.tsv"
while kill -0 $pid 2>/dev/null; do
  sleep 30; t=$((t+30))
  kill -0 $pid 2>/dev/null || break
  lat=$(curl -s -o /dev/null -m 2 -w '%{time_connect}' https://1.1.1.1/ || echo 9)
  cpu=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)
  st=$(psql "$DATABASE_URL" -Atc "select coalesce(string_agg(discovered_via||'='||n, ','),'') from (select discovered_via, count(*) n from dht_torrents group by 1) x" 2>/dev/null)
  printf '%s\t%s\t%s\t%s\n' "$t" "$lat" "$cpu" "$st" >> "$out/samples.tsv"
  if awk -v l="$lat" 'BEGIN{exit !(l>0.1)}'; then bad=$((bad+1)); else bad=0; fi
  if [ $bad -ge 2 ]; then echo "latency guard: stopping at ${t}s" | tee "$out/stopped.txt"; kill -INT $pid; fi
done
wait $pid; echo "exit $?" >> "$out/start.txt"
date -Is >> "$out/start.txt"
psql "$DATABASE_URL" -Atc "select discovered_via, count(*) from dht_torrents group by 1" > "$out/stored_by_via.txt"
psql $PGU/postgres -qc "drop database if exists $db"
