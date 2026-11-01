#!/usr/bin/env python3
"""Router ramp with the real worker: runs `magnetar-server worker --seconds STEP` at rising
MAGNETAR_DHT_QPS and stops at the first fault (no retry, no higher step).
Usage: worker_bench.py <env file> <gateway ip> <out.jsonl> <step secs> <rate,rate,...> [VARS]
  VARS (optional): comma-separated variable names; each step is then "a/b/c" setting them in
  order (e.g. MAGNETAR_DHT_TCP_RATE,MAGNETAR_DHT_FETCH_CONCURRENCY with steps 150/128,200/160).
Fault (12 s window, 2 s probes, TCP connect, 1 s timeout):
  gateway :80 fails 3 times in a row (only if it answered during the baseline);
  1.1.1.1:443 and 9.9.9.9:443 both fail in >= 3 of the last 6 samples;
  1.1.1.1 median of the last 6 > max(3 x baseline median, baseline + 40 ms);
  gateway :80 median of the last 6 > max(5 x baseline median, baseline + 10 ms) (router CPU);
  nf_conntrack_count > 90 % of max; default route changed; worker exited early.
The worker always ends by itself after STEP seconds, so a dead bench cannot leave it running."""
import json, os, signal, socket, statistics, subprocess, sys, time

ENV, GW, OUT, STEP, RATES = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5].split(",")
VARS = sys.argv[6].split(",") if len(sys.argv) > 6 else ["MAGNETAR_DHT_QPS"]
BIN = os.path.expanduser("~/magnetar-server")


def tcp(host, port):
    t = time.monotonic()
    try:
        with socket.create_connection((host, port), timeout=1.0):
            return (time.monotonic() - t) * 1000
    except OSError:
        return None


def conntrack():
    try:
        c = int(open("/proc/sys/net/netfilter/nf_conntrack_count").read())
        m = int(open("/proc/sys/net/netfilter/nf_conntrack_max").read())
        return c, m
    except OSError:
        return None, None


def route():
    return subprocess.run(["ip", "route", "show", "default"], capture_output=True, text=True).stdout.strip()


def log(rec):
    rec["t"] = time.strftime("%H:%M:%S")
    with open(OUT, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps(rec), flush=True)


def sample():
    c, m = conntrack()
    return {"gw": tcp(GW, 80), "cf": tcp("1.1.1.1", 443), "q9": tcp("9.9.9.9", 443), "ct": c, "ctmax": m}


env = dict(os.environ)
for line in open(ENV):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        env[k] = v

# Baseline: 60 s, nothing running.
base = [sample() for _ in range(30) if time.sleep(2) is None]
gw_ok = sum(s["gw"] is not None for s in base) >= 27
cf = [s["cf"] for s in base if s["cf"] is not None]
cf_med = statistics.median(cf) if cf else 50.0
lat_limit = max(3 * cf_med, cf_med + 40)
gws = [s["gw"] for s in base if s["gw"] is not None]
gw_med = statistics.median(gws) if gws else 1.0
gw_limit = max(5 * gw_med, gw_med + 10)
route0 = route()
log({"phase": "baseline", "gw_answers": gw_ok, "cf_med": round(cf_med, 1), "lat_limit": round(lat_limit, 1), "gw_med": round(gw_med, 2), "gw_limit": round(gw_limit, 1),
     "ct": base[-1]["ct"], "ctmax": base[-1]["ctmax"]})

max_ok = None
for rate in RATES:
    vals = rate.split("/")
    e = dict(env, **dict(zip(VARS, vals)))
    rep = open(OUT + f".worker-{rate.replace('/', '_')}.log", "w")
    p = subprocess.Popen([BIN, "worker", "--seconds", str(STEP)], env=e, stdout=rep, stderr=subprocess.STDOUT,
                         start_new_session=True)
    win, gw_fail, fault, t0 = [], 0, None, time.monotonic()
    while time.monotonic() - t0 < STEP - 5:
        time.sleep(2)
        s = sample()
        win = (win + [s])[-6:]
        gw_fail = gw_fail + 1 if (gw_ok and s["gw"] is None) else 0
        lost = sum(x["cf"] is None and x["q9"] is None for x in win)
        meds = [x["cf"] for x in win if x["cf"] is not None]
        med = statistics.median(meds) if meds else None
        gmeds = [x["gw"] for x in win if x["gw"] is not None]
        gmed = statistics.median(gmeds) if gmeds else None
        if p.poll() is not None:
            fault = f"worker exited early ({p.returncode})"
        elif gw_fail >= 3:
            fault = "gateway TCP:80 failed 3 times in a row"
        elif lost >= 3:
            fault = f"1.1.1.1 and 9.9.9.9 lost in {lost}/6"
        elif len(win) == 6 and med is not None and med > lat_limit:
            fault = f"1.1.1.1 median {med:.0f} ms > {lat_limit:.0f}"
        elif gw_ok and len(win) == 6 and gmed is not None and gmed > gw_limit:
            fault = f"gateway median {gmed:.1f} ms > {gw_limit:.1f}"
        elif s["ct"] and s["ctmax"] and s["ct"] > 0.9 * s["ctmax"]:
            fault = f"conntrack {s['ct']}/{s['ctmax']}"
        elif route() != route0:
            fault = "default route changed"
        if int(time.monotonic() - t0) % 30 < 2 or fault:
            log({"phase": "step", "rate": rate, "s": int(time.monotonic() - t0), "cf_med": med and round(med, 1),
                 "lost": lost, "gw": s["gw"] and round(s["gw"], 1), "gw_med": gmed and round(gmed, 2), "ct": s["ct"], "fault": fault})
        if fault:
            break
    if fault:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        p.wait()
        log({"phase": "fault", "rate": rate, "fault": fault, "max_ok": max_ok})
        break
    p.wait()
    max_ok = rate
    log({"phase": "pass", "rate": rate})
else:
    log({"phase": "done", "max_ok": max_ok, "note": "no fault up to the last step"})
