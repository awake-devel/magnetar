#!/bin/bash
# ab.sh "nodes qps lookups" ... : engine bench, old then new, with UDP receive-buffer drops.
S=$HOME/.cache/magnetar-cores-scratch
rcv() { awk '/^Udp:/{n++; if(n==2) print $6}' /proc/net/snmp; }
for cfg in "$@"; do set -- $cfg
  for b in base new; do
    r0=$(rcv)
    out=$(DHT_BENCH_NODES=$1 DHT_BENCH_QPS=$2 DHT_BENCH_LOOKUPS=$3 DHT_BENCH_SECS=${SECS:-15} DHT_BENCH_GHOSTS=${GHOSTS:-250} timeout 90 $S/bench-$b dht_bench:: --ignored --nocapture 2>&1 | grep BENCH)
    echo "$b $out udp_rcvbuf_drops=$(( $(rcv) - r0 ))"
  done
done
