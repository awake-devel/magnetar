#!/bin/bash
# run.sh NAME BINARY NODES QPS SECONDS PORT [extra env...]
# One live `dht` run against a fresh scratch database, with a 10 s sampler (CPU, threads, RSS,
# conntrack, 1.1.1.1 connect time) and the latency guard (2 × > 0.1 s in a row → SIGINT).
set -u
NAME=$1 BIN=$2 NODES=$3 QPS=$4 SECS=$5 PORT=$6; shift 6
S=$HOME/.cache/magnetar-cores-scratch
R=$S/runs/$NAME; mkdir -p $R
src=$(ip route get 1.1.1.1 | grep -o 'src [0-9.]*' | cut -d' ' -f2)
if [ "$src" != "<ip>" ]; then echo "NETWORK CHECK FAILED: src=$src" | tee $R/aborted; exit 3; fi
DB=cores_${NAME//[^a-z0-9]/_}
docker exec magnetar-pg dropdb -U magnetar --if-exists $DB >/dev/null
docker exec magnetar-pg createdb -U magnetar $DB
export DATABASE_URL=postgres://magnetar:magnetar@127.0.0.1:54329/$DB
export XDG_STATE_HOME=$R/state MAGNETAR_DHT_UPNP=false RUST_LOG=info MAGNETAR_GEOIP=off
export MAGNETAR_DHT_PORT=$PORT MAGNETAR_DHT_NODES=$NODES MAGNETAR_DHT_QPS=$QPS
export MAGNETAR_DHT_PEER_LOOKUPS=${LOOKUPS:-2048} MAGNETAR_DHT_FETCH_CONCURRENCY=2048
export MAGNETAR_DHT_TCP_RATE=1000 MAGNETAR_DHT_TCP_MAX=4000
for kv in "$@"; do export "$kv"; done
env | grep '^MAGNETAR_DHT' | sort > $R/env.txt
date -Is > $R/start
$BIN dht --mode aggressive --seconds $SECS > $R/report.json 2> $R/stderr.log &
PID=$!
echo $PID > $R/pid
bad=0; prev_cpu=0; prev_t=$(date +%s.%N)
echo "t cpu_cores rss_mb threads_top conntrack connect_s other_crawlers" > $R/samples.txt
while kill -0 $PID 2>/dev/null; do
  sleep 10
  kill -0 $PID 2>/dev/null || break
  now=$(date +%s.%N)
  cpu=$(awk '{print $14+$15}' /proc/$PID/stat 2>/dev/null || echo 0)
  rss=$(awk '/VmRSS/{print int($2/1024)}' /proc/$PID/status 2>/dev/null)
  hz=$(getconf CLK_TCK)
  cores=$(awk -v a=$cpu -v b=$prev_cpu -v h=$hz -v n=$now -v p=$prev_t 'BEGIN{printf "%.2f", (a-b)/h/(n-p)}')
  prev_cpu=$cpu; prev_t=$now
  top=$(for t in /proc/$PID/task/*; do awk '{print $14+$15, $2}' $t/stat 2>/dev/null; done | sort -rn | head -4 | tr '\n' ' ')
  ct=$(cat /proc/sys/net/netfilter/nf_conntrack_count 2>/dev/null || echo na)
  c=$(curl -s -m 2 -o /dev/null -w '%{time_connect}' https://1.1.1.1/ || echo 2)
  other=$(pgrep -f '^[^ ]+ dht --mode' | grep -vx $PID | wc -l)
  echo "$(date +%T) $cores $rss [$top] $ct $c $other" >> $R/samples.txt
  if awk -v c=$c 'BEGIN{exit !(c>0.1)}'; then bad=$((bad+1)); else bad=0; fi
  if [ $bad -ge 2 ]; then echo "LATENCY GUARD at $(date +%T): $c" | tee -a $R/guard; kill -INT $PID; fi
done
wait $PID; echo "exit $?" > $R/exit
date -Is > $R/end
