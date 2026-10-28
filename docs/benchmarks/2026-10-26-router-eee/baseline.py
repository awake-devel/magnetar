"""EEE baseline: idle probe stats (written to baseline.json, used by the ramps as reference),
background interface traffic, then throughput (single + 4-parallel down, single + 2-parallel up)
with probes still running to see latency under load."""
import sys, time, json, subprocess, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mon import Monitor, BASE

DUR = int(sys.argv[1]) if len(sys.argv) > 1 else 60
IFACE = "enp3s0"

def ifbytes():
    for line in open("/proc/net/dev"):
        if line.strip().startswith(IFACE + ":"):
            v = line.split(":")[1].split()
            return int(v[0]), int(v[8])
    return 0, 0

def down(n, nbytes=25_000_000, reps=6, maxt=25):
    # Cloudflare refuses >~25 MB per request (403), so each stream fetches `reps` x 25 MB
    t = time.time()
    url = f"https://speed.cloudflare.com/__down?bytes={nbytes}"
    ps = [subprocess.Popen(["curl", "-s", "--max-time", str(maxt), "-w", "%{size_download}\\n"]
          + sum([["-o", "/dev/null", url] for _ in range(reps)], []),
          stdout=subprocess.PIPE, text=True) for _ in range(n)]
    got = sum(sum(int(float(x)) for x in p.communicate()[0].split()) for p in ps)
    return round(got * 8 / (time.time() - t) / 1e6, 1)

def up(n, nbytes=50_000_000, maxt=25):
    t = time.time()
    cmd = (f"head -c {nbytes} /dev/zero | curl -s -o /dev/null -w '%{{size_upload}}' "
           f"--data-binary @- https://speed.cloudflare.com/__up --max-time {maxt}")
    ps = [subprocess.Popen(cmd, shell=True, stdout=subprocess.PIPE, text=True) for _ in range(n)]
    got = sum(int(float(p.communicate()[0] or 0)) for p in ps)
    return round(got * 8 / (time.time() - t) / 1e6, 1)

m = Monitor(); t0 = time.time(); m.start()
if "--tp-only" not in sys.argv:
    rx0, tx0 = ifbytes()
    print(f"baseline: idle sampling {DUR}s...", flush=True)
    time.sleep(DUR)
    rx1, tx1 = ifbytes(); t1 = time.time()
    res = {k: m.stats(k, since=t0) for k in m.keys}
    res["cpu"] = m.cpu_stats(since=t0)
    res["background_mbps"] = {"rx": round((rx1 - rx0) * 8 / (t1 - t0) / 1e6, 1),
                              "tx": round((tx1 - tx0) * 8 / (t1 - t0) / 1e6, 1)}
    json.dump(res, open(os.path.join(BASE, "baseline.json"), "w"), indent=2)
    print(json.dumps(res, indent=1), flush=True)

if "--no-tp" not in sys.argv:
    tp = {}
    for name, fn in (("down_1", lambda: down(1)), ("down_4", lambda: down(4)),
                     ("up_1", lambda: up(1)), ("up_2", lambda: up(2))):
        ts = time.time()
        tp[name] = fn()
        tp[name + "_probe"] = {"tcp": m.stats("tcp", since=ts), "svc": m.stats("svc", since=ts),
                               "icmp": m.stats("icmp", since=ts)}
        print(name, tp[name], "Mbps", tp[name + "_probe"], flush=True)
        time.sleep(5)
    tp["note"] = (f"background traffic during idle sample: {res['background_mbps']} Mbps "
                  "(user services, mostly radicle-node seeding); results are on top of it")
    json.dump(tp, open(os.path.join(BASE, "throughput.json"), "w"), indent=2)
m.stop()
print("DONE", flush=True)
