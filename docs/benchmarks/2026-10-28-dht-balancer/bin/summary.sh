#!/usr/bin/env bash
cd ~/.cache/magnetar-balance-scratch/logs
printf 'run\tstart\tstored/min\thashes/min\tmeta/min\tsample_q\tget_peers_q\tsample_share_of_q\tpaused_s\tqueue_end(sampled)\tq/s\tcpu\tshare_end\tyield_s/kq\tlat_max\n'
for d in main1 new1 main2 new2 main3 new3 main4 new4; do
  [ -s $d/report.json ] || continue
  st=$(head -1 $d/start.txt | cut -c12-16)
  lat=$(cut -f2 $d/samples.tsv | sort -g | tail -1)
  jq -r --arg d $d --arg st $st --arg lat $lat '[$d,$st,
   (.persist.stored/(.seconds/60)|floor), (.unique_hashes_per_min|floor), (.metadata_per_min|floor),
   .engine.sent_sample_infohashes, .engine.sent_get_peers,
   ((.engine.sent_sample_infohashes/(.engine.sent_sample_infohashes+.engine.sent_get_peers))*1000|floor/1000),
   .sampling_paused_secs, "\(.queue_len)(\(.queue_lanes[1]))", (.queries_per_sec|floor),
   ((.cpu_seconds/.seconds)*100|floor/100), (.balance.share // "-"), ((.stored_per_kq[0] // 0)*100|floor/100), $lat] | @tsv' $d/report.json
done
