"""Pi baseline: idle probe stats (30 s), then throughput (curl to speed.cloudflare.com) with
latency-under-load and Pi CPU per test. Writes baseline.json next to the script."""
import sys, time, json, subprocess, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mon import Monitor, BASE

IDLE = int(sys.argv[1]) if len(sys.argv) > 1 else 30
DL = "https://speed.cloudflare.com/__down?bytes=%d"


def curl_down(n, nbytes, maxt=40):
    t = time.time()
    ps = [subprocess.Popen(["curl", "-s", "-o", "/dev/null", "-w", "%{size_download}", DL % nbytes,
                            "--max-time", str(maxt)], stdout=subprocess.PIPE, text=True) for _ in range(n)]
    got = sum(float(p.communicate()[0] or 0) for p in ps)
    return round(got * 8 / (time.time() - t) / 1e6, 1)


def curl_up(n, nbytes, maxt=40):
    t = time.time()
    cmd = (f"head -c {nbytes} /dev/zero | curl -s -o /dev/null -w '%{{size_upload}}' "
           f"--data-binary @- https://speed.cloudflare.com/__up --max-time {maxt}")
    ps = [subprocess.Popen(cmd, shell=True, stdout=subprocess.PIPE, text=True) for _ in range(n)]
    got = sum(float(p.communicate()[0] or 0) for p in ps)
    return round(got * 8 / (time.time() - t) / 1e6, 1)


m = Monitor(); m.start()
t0 = time.time()
print(f"baseline: idle sampling {IDLE}s...", flush=True)
time.sleep(IDLE)
res = {"idle": {k: m.stats(k, since=t0) for k in m.keys}, "idle_res": m.res_stats(since=t0)}
print(json.dumps(res, indent=1), flush=True)
tests = [("down_1x", lambda: curl_down(1, 40_000_000)),
         ("down_4x", lambda: curl_down(4, 25_000_000)),
         ("up_1x", lambda: curl_up(1, 25_000_000)),
         ("up_3x", lambda: curl_up(3, 12_000_000))]
res["throughput"] = {}
for name, fn in tests:
    ts = time.time()
    try: mbps = fn()
    except Exception as e: mbps = f"err:{e}"
    te = time.time()
    res["throughput"][name] = {"mbps": mbps, "secs": round(te - ts, 1),
                               "tcp_loaded": m.stats("tcp", ts, te), "gw_loaded": m.stats("gwtcp", ts, te),
                               "icmp_loaded": m.stats("icmp", ts, te), "pi": m.res_stats(ts, te)}
    print(name, json.dumps(res["throughput"][name]), flush=True)
    time.sleep(4)
m.stop()
json.dump(res, open(os.path.join(BASE, "baseline.json"), "w"), indent=1)
print("DONE", flush=True)
