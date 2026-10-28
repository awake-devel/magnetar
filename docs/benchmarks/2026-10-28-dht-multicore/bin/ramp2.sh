#!/bin/bash
# ramp.sh "name bin nodes qps port" ... : sequential runs; stops after a run hit the latency guard
# or the network check failed.
S=$HOME/.cache/magnetar-cores-scratch
for spec in "$@"; do set -- $spec
  cp $S/bin/run.sh $S/bin/run-$1.sh
  bash $S/bin/run-$1.sh $1 $S/bins/$2 $3 $4 600 $5 "${@:6}" > $S/runs/$1.out 2>&1
  rc=$?
  echo "$(date +%T) $1 done rc=$rc" >> $S/runs/ramp.log
  if [ $rc -eq 3 ] || [ -f $S/runs/$1/guard ]; then echo "stop after $1" >> $S/runs/ramp.log; exit 1; fi
done
