#!/bin/bash
# nodes-ab.sh N1 N2 ...: 10-min production runs per DHT node count; raw samples as JSON lines.
OUT=$HOME/magnetar/docs/benchmarks/2026-10-28-dht-nodes-4-vs-16/runs.jsonl
ENV=$HOME/.config/magnetar/compose.env
for n in "$@"; do
  [ "$(ip route get 1.1.1.1 | grep -o 'src [0-9.]*')" = "src 80.119.126.13" ] || { echo '{"stopped":"not on fiber"}' >> $OUT; break; }
  sed -i '/^MAGNETAR_DHT_NODES=/d' $ENV
  sed -i "/^MAGNETAR_DHT_UPNP=false$/a MAGNETAR_DHT_NODES=$n" $ENV
  magnetar-host compose up -d --no-build magnetar >/dev/null 2>&1
  start=$(date -u +%FT%TZ); P=$(docker inspect -f '{{.State.Pid}}' magnetar-magnetar-1); T=$(magnetar-host token)
  for i in 1 2 3 4 5; do sleep 120
    d=$(curl -s -m 30 -H "Authorization: Bearer $T" https://ryoku.tail26814a.ts.net/api/v1/stats | jq -c '.dht | {uptime:.uptime_secs, nodes, qps:(.queries_per_sec|floor), answered:(.answered_share*1000|floor/1000), brake, hashes_per_min:(.hashes_per_min|floor), metadata_per_min:(.metadata_per_min|floor), fetch_ok:(.fetch_success_rate*1000|floor/1000), stored, queue:.queue_len, sampling_paused}')
    ct=$(cat /proc/sys/net/netfilter/nf_conntrack_count); cpu=$(ps -o %cpu= -p $P | tr -d ' '); rss=$(ps -o rss= -p $P | tr -d ' '); lat=$(curl -s -o /dev/null -w %{time_connect} https://1.1.1.1/)
    echo "$d" | jq -c --arg run "nodes=$n" --arg start "$start" --argjson s $i --argjson ct $ct --arg cpu "$cpu" --argjson rss $rss --arg lat "$lat" '{run:$run, start:$start, sample:$s} + . + {conntrack:$ct, cpu_pct:($cpu|tonumber), rss_mb:($rss/1024|floor), connect_1111_s:($lat|tonumber)}' >> $OUT
  done
done
sed -i '/^MAGNETAR_DHT_NODES=/d' $ENV; echo restored-4-nodes
