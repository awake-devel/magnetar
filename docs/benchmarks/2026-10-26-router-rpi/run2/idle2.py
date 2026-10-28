"""Run 2: 20 s idle re-check vs baseline.json (not overwritten), + throttled/dmesg snapshot."""
import sys, os, time, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mon import Monitor, BL, throttled, ctrack
m = Monitor(); m.start(); t0 = time.time(); time.sleep(20); m.stop()
out = {k: m.stats(k, since=t0) for k in m.keys}; out["pi"] = m.res_stats(since=t0)
out["throttled"] = hex(throttled()); out["baseline_med"] = BL
print(json.dumps(out, indent=1))
