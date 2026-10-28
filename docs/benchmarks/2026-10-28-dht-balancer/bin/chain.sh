#!/usr/bin/env bash
cd ~/.cache/magnetar-balance-scratch
for r in "$@"; do
  set -- $r
  bin/run.sh $1 $2 $3 > logs/$1.out 2>&1 || { echo "run $1 failed"; cat logs/$1.out; exit 1; }
done
echo done
