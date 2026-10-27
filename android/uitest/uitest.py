#!/usr/bin/env python3
"""Emulator UI test of the app's main flows (docs/APP.md §11), driven from the host over adb.

No test library inside the app: the instrumented-test stack (androidx.test runner, espresso,
compose ui-test-junit4) pulls JUnit 4, whose EPL licence is outside the allowed list (D80). The
app turns its Compose test tags into resource ids (`testTagsAsResourceId`), and this script
reads `uiautomator dump`, taps and types with `input`, and reads the app's `MagnetarTiming`
log lines (milliseconds only, no query text or titles).

    python3 android/uitest/uitest.py [--serial emulator-5554] [--production URL]

Flows on the bundled fake server (fake_server.py, which also takes the writes): search →
detail → title page, filters apply, saved search create (with its filter), deep link. With
--production (read-only: searches, facets, detail, releases; no Check now, no saved search),
also search → detail → title page there and the time to first results; the API token is read
from $MAGNETAR_API_TOKEN or ~/.config/magnetar/compose.env and never printed.

Only emulator serials are accepted, so a real phone on the same adb is never touched.
"""
import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fake_server  # noqa: E402

PKG = "app.magnetar"
SERIAL = "emulator-5554"


def adb(*args, check=True, capture=True):
    cmd = ["adb", "-s", SERIAL, *args]
    r = subprocess.run(cmd, capture_output=capture, text=True, timeout=120)
    if check and r.returncode != 0:
        raise RuntimeError("adb failed: " + " ".join(args[:3]) + " " + (r.stderr or "")[:200])
    return r.stdout if capture else ""


def shell(cmd, check=True):
    return adb("shell", cmd, check=check)


def set_server(url, token):
    """Writes the app's settings file (docs/APP.md §7) with run-as; the token goes only into it."""
    xml = (
        "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n"
        f'    <string name="server_url">{escape(url)}</string>\n'
        f'    <string name="api_token">{escape(token)}</string>\n'
        '    <int name="settings_version" value="1" />\n'
        '    <int name="trust_threshold" value="20" />\n'
        '    <boolean name="show_adult" value="false" />\n'
        '    <boolean name="notify_saved_searches" value="false" />\n'
        "</map>\n"
    )
    shell(f"am force-stop {PKG}")
    fd, path = tempfile.mkstemp(dir=os.path.expanduser("~/.cache"), prefix="uitest-", suffix=".xml")
    try:
        with os.fdopen(fd, "w") as f:
            f.write(xml)
        adb("push", path, "/data/local/tmp/uitest-settings.xml")
    finally:
        os.unlink(path)
    shell(f"run-as {PKG} sh -c 'mkdir -p shared_prefs && cp /data/local/tmp/uitest-settings.xml shared_prefs/settings.xml'")
    shell("rm -f /data/local/tmp/uitest-settings.xml")
    # A fresh start: no cached pages or posters of another server.
    shell(f"run-as {PKG} sh -c 'rm -rf cache/pages cache/posters'", check=False)


def launch(deep_link=None):
    shell("logcat -c")
    if deep_link:
        shell(f"am start -W -a android.intent.action.VIEW -d {deep_link} {PKG}")
    else:
        shell(f"am start -W -n {PKG}/.MainActivity")


class Node:
    def __init__(self, e):
        self.id = e.get("resource-id", "")
        self.text = e.get("text", "")
        self.desc = e.get("content-desc", "")
        self.enabled = e.get("enabled") == "true"
        x1, y1, x2, y2 = map(int, re.findall(r"\d+", e.get("bounds", "[0,0][0,0]")))
        self.center = ((x1 + x2) // 2, (y1 + y2) // 2)
        self.top = y1


def dump():
    for _ in range(3):
        out = shell("uiautomator dump /sdcard/uitest.xml >/dev/null 2>&1; cat /sdcard/uitest.xml", check=False)
        if "<hierarchy" in out:
            root = ET.fromstring(out[out.index("<?xml"):] if "<?xml" in out else out)
            nodes = [Node(e) for e in root.iter("node")]
            # A slow emulator's "System UI isn't responding": wait, it is not the app.
            anr = [n for n in nodes if n.id == "android:id/aerr_wait"]
            if anr:
                tap(anr[0])
                time.sleep(1.0)
                continue
            return nodes
        time.sleep(0.5)
    return []


def find(nodes, rid=None, text_re=None):
    return [n for n in nodes if (rid is None or n.id == rid) and (text_re is None or re.search(text_re, n.text))]


def wait(rid=None, text_re=None, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        hits = find(dump(), rid, text_re)
        if hits:
            return hits
        time.sleep(0.3)
    raise AssertionError(f"timed out waiting for {rid or ''} {text_re or ''}")


def scroll_to(rid, tries=8):
    """Swipes up until [rid] is on screen (uiautomator lists visible nodes only)."""
    for _ in range(tries):
        hits = find(dump(), rid)
        if hits:
            return hits
        # A slow swipe (no fling), then let it settle so the coordinates hold.
        shell("input swipe 540 1700 540 900 600")
        time.sleep(0.8)
    raise AssertionError("not found after scrolling: " + rid)


def tap(n):
    shell(f"input tap {n.center[0]} {n.center[1]}")


def type_text(text):
    shell("input text " + text.replace(" ", "%s"))


def back():
    shell("input keyevent 4")


def timings():
    out = shell("logcat -d -s MagnetarTiming:I", check=False)
    return [(int(m.group(1)), m.group(2), int(m.group(3)))
            for m in re.finditer(r"MagnetarTiming: (\d+) (live|cached) len=(\d+)", out)]


def wait_timing(kind, after, length=None, timeout=20):
    """The first [kind] timing after index [after] (for a query of [length] characters)."""
    end = time.time() + timeout
    while time.time() < end:
        t = [ms for ms, k, n in timings()[after:] if k == kind and (length is None or n == length)]
        if t:
            return t[0]
        time.sleep(0.2)
    raise AssertionError("no " + kind + " result within the timeout")


def clear_search():
    box = wait("search_box")[0]
    tap(box)
    shell("input keyevent KEYCODE_MOVE_END")
    shell("input keyevent --longpress " + " ".join(["KEYCODE_DEL"] * 40), check=False)


passed = []


def ok(name):
    passed.append(name)
    print("PASS", name, flush=True)


def flow_search_detail_title(expect_media=True):
    """Search → first result with a title link → detail (media card) → All releases → title page."""
    rows = wait("media_line", timeout=25)
    # Open the detail of a row that has a title link (tap its title area, below the media line).
    row = rows[0]
    shell(f"input tap {row.center[0]} {row.center[1] + 90}")
    wait("detail_title", timeout=25)
    ok("search -> detail")
    nodes = dump()
    if expect_media:
        # The media card can be below the fold on a small screen.
        for _ in range(3):
            if find(nodes, "all_releases"):
                break
            shell("input swipe 500 1500 500 700 300")
            nodes = dump()
        tap(find(nodes, "all_releases")[0])
        wait("title_page", timeout=25)
        wait("title_group", timeout=25)
        ok("detail -> title page")
        back()
    wait("detail_title")
    back()
    wait("search_box")


def run_fake(port):
    srv = fake_server.serve(port)
    base = f"http://127.0.0.1:{port}"
    set_server(f"http://10.0.2.2:{port}", fake_server.TOKEN)
    launch()
    wait("result_count", timeout=30)
    ok("fake: browse lists results")

    tap(wait("search_box")[0])
    type_text("bunny")
    wait("result_count", r"^1 result")
    ok("fake: instant search (debounced)")
    flow_search_detail_title()

    # Filters apply: the 1080p chip is sent as resolutions.
    tap(wait("filters")[0])
    chip = wait("chip_resolution_1080p")[0]
    # The chip's label is a child node: the server's count is shown next to the value.
    wait(text_re=r"^1080p \(3\)$")
    tap(chip)
    tap(scroll_to("filters_apply")[0])
    wait("search_box")
    time.sleep(1.0)
    last = json.load(urllib.request.urlopen(base + "/_uitest/searches"))[-1]
    assert last["resolutions"] == ["1080p"] and last["text"] == "bunny", "filter sent"
    assert last["include_adult"] is False, "adult switch sent (off)"
    ok("fake: filter sheet counts and apply")

    # Back on the sheet applies too.
    tap(wait("filters")[0])
    tap(wait("chip_origin_dht")[0])
    back()
    wait("search_box")
    time.sleep(1.0)
    last = json.load(urllib.request.urlopen(base + "/_uitest/searches"))[-1]
    assert last["origins"] == ["dht"], "Back applied the Source chip"
    ok("fake: Back applies the sheet")
    tap(wait("filters")[0])
    tap(wait("chip_origin_dht")[0])
    tap(scroll_to("filters_apply")[0])
    wait("search_box")

    # Save the current search (text + filter) on the server, then see it on the Saved tab.
    tap(wait("save_search")[0])
    tap(wait("save_confirm")[0])
    time.sleep(1.0)
    saved = json.load(urllib.request.urlopen(base + "/_uitest/saved"))
    assert len(saved) == 1 and saved[0]["name"] == "bunny", "saved by name"
    assert saved[0]["query"]["resolutions"] == ["1080p"] and saved[0]["notify"] is True, "filters saved"
    tap(wait("tab_saved")[0])
    wait("saved_item")
    ok("fake: saved search created with its filters, listed on Saved")
    tap(wait("tab_search")[0])

    # Notification deep link → detail.
    shell(f"am force-stop {PKG}")
    launch(deep_link="magnetar://torrent/" + fake_server.HITS[1]["infohash"])
    wait("detail_title", timeout=25)
    ok("fake: magnetar://torrent deep link opens the detail")
    srv.shutdown()


def production_token():
    t = os.environ.get("MAGNETAR_API_TOKEN")
    if t:
        return t
    path = os.path.expanduser("~/.config/magnetar/compose.env")
    with open(path) as f:
        for line in f:
            if line.startswith("MAGNETAR_API_TOKEN="):
                return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit("no MAGNETAR_API_TOKEN")


def run_production(url, queries):
    """Read-only: searches, facets, a detail and a title page. Prints timings only."""
    set_server(url, production_token())
    t0 = time.time()
    launch()
    first = wait_timing("live", 0, timeout=30)
    cold = int((time.time() - t0) * 1000)
    print(f"production: cold start to first results (browse) {cold} ms wall, {first} ms in-app", flush=True)
    live = []
    for q in queries:
        clear_search()
        before = len(timings())
        tap(wait("search_box")[0])
        type_text(q)
        try:
            live.append(wait_timing("live", before, len(q)))
        except AssertionError:
            # A transient server error (a redeploy answers 502): Retry once, not counted.
            print("production: no answer for one query, retrying", flush=True)
            tap(wait("search_box")[0])
            shell("input keyevent 66")
            wait_timing("live", before, len(q))
        wait("result_count")
    print("production: keystroke-to-results (incl. 250 ms debounce) ms:", live,
          f"median {statistics.median(live)} max {max(live)}", flush=True)
    ok(f"production: {len(queries)} searches answered")
    clear_search()
    tap(wait("search_box")[0])
    type_text(queries[0])
    wait("result_count")
    flow_search_detail_title()
    tap(wait("filters")[0])
    wait("chip_origin_dht", timeout=20)
    wait(text_re=r"^DHT \(\d[\d,]*\)$", timeout=20)
    ok("production: filter sheet shows live counts")
    back()
    wait("search_box")
    return live


def main():
    global SERIAL
    p = argparse.ArgumentParser()
    p.add_argument("--serial", default=SERIAL)
    p.add_argument("--port", type=int, default=18787)
    p.add_argument("--production")
    p.add_argument("--queries", default="dune,matrix,ubuntu,interstellar,debian,blade runner")
    a = p.parse_args()
    if not a.serial.startswith("emulator-"):
        raise SystemExit("refusing a non-emulator device: " + a.serial)
    SERIAL = a.serial
    # Belt and braces: besides the serial name, the device itself must say it is an emulator.
    qemu = (shell("getprop ro.kernel.qemu", check=False).strip(), shell("getprop ro.boot.qemu", check=False).strip())
    if "1" not in qemu:
        raise SystemExit("refusing a device that is not an emulator: " + a.serial)
    run_fake(a.port)
    if a.production:
        run_production(a.production, a.queries.split(","))
    print(f"{len(passed)} checks passed")


if __name__ == "__main__":
    main()
