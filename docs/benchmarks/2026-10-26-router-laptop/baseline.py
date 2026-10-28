import sys, time, json, subprocess, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mon import Monitor

DUR = int(sys.argv[1]) if len(sys.argv) > 1 else 60

def throughput():
    out = {}
    try:
        t = time.time()
        ps = [subprocess.Popen(["curl", "-s", "-o", "/dev/null",
              "https://speed.cloudflare.com/__down?bytes=50000000", "--max-time", "30"]) for _ in range(4)]
        [p.wait() for p in ps]
        out["down_mbps_agg"] = round(200 * 8 / (time.time() - t), 0)
    except Exception as e:
        out["down_mbps_agg"] = f"err:{e}"
    try:
        r = subprocess.run("head -c 50000000 /dev/zero | curl -s -o /dev/null -w '%{speed_upload}' "
                           "--data-binary @- https://speed.cloudflare.com/__up --max-time 25",
                           shell=True, capture_output=True, text=True, timeout=30)
        out["up_mbps"] = round(float(r.stdout.strip()) * 8 / 1e6, 0)
    except Exception as e:
        out["up_mbps"] = f"err:{e}"
    return out

m = Monitor()
t0 = time.time()
m.start()
print(f"baseline: sampling {DUR}s...", flush=True)
time.sleep(DUR * 0.5)
tp = throughput()
time.sleep(max(2, DUR - (time.time() - t0)))
m.stop()
res = {k: m.stats(k, since=t0) for k in m.keys}
res["throughput"] = tp
res["throughput"]["note"] = "single-stream download ~230 Mbps measured separately"
print(json.dumps(res, indent=2))
json.dump(res, open(os.path.expanduser("~/.cache/router-bench/baseline.json"), "w"), indent=2)
