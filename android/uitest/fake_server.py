#!/usr/bin/env python3
"""A tiny stand-in Magnetar server for the emulator UI test (android/uitest/uitest.py).

Serves canned answers for /health, search, facets, detail, releases and the saved-search CRUD,
so the flows that write (saving a search) never touch a real server. Python standard library
only. Listens on 127.0.0.1; the emulator reaches it as http://10.0.2.2:<port>.
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = "uitest-token"
FEATURES = ["filters", "facet_set", "media_releases", "liveness_history", "adult_filter", "origin", "dht",
            "saved_search_poll", "browse_checked_dht", "stats", "graphs"]
ROOT_HEX = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"


def hit(i, title, **kw):
    h = {"infohash": "%040x" % (0xabc000 + i), "event_id": "", "title": title, "category": "video",
         "total_size": 1_500_000_000 + i, "file_count": 2, "created_at": 1_790_000_000 - i * 3600, "pubkey": "",
         "npub": "", "seeders": 10 + i, "leechers": 2, "dead": False, "trust": 40, "pow_bits": 0,
         "publisher_count": 1, "rank": 1.0, "pinned": False, "hidden": False, "origin": "nostr",
         "attrs": {"resolution": "1080p", "source": "web-dl", "video_codec": "h264", "languages": ["fr"],
                   "french": ["vff"]}}
    h.update(kw)
    return h


HITS = [
    hit(1, "Big.Buck.Bunny.2008.1080p.WEB-DL.H264-UITEST", media_title="Big Buck Bunny", media_year=2008,
        media_id=42, media_rating=6.4, media_kind="movie", matched_file=None),
    hit(2, "Sintel.2010.2160p.WEB-DL.H265-UITEST", origin="dht"),
    hit(3, "Tears.of.Steel.2012.720p.WEB-DL.H264-UITEST", origin="both"),
]


class State:
    lock = threading.Lock()
    saved = []
    next_id = 1
    requests = []
    searches = []
    breakdowns = []
    trust = {"root": None, "threshold": 20}
    trust_puts = []


STATS_HEALTH = {
    "torrents": 1000, "media_linked": 400, "attrs_missing": 0, "computed_at": 1_790_000_000,
    "liveness": {"enabled": True, "torrents": 1000, "never_checked": 380, "due_now": 450, "checked_24h": 600,
                 "checks_per_hour": 30, "alive": 350, "dead": 240, "unknown": 30, "trackers": 9,
                 "trackers_backed_off": 1, "dht_nodes": 100, "last_batch": {"hashes": 200, "ms": 60000},
                 "last_batch_at": 1_789_999_000},
    "search_index": {"state": "ready", "docs": 1000, "pending": 3, "lag_secs": 2, "bytes": 1_000_000},
    "relays": {"total": 10, "enabled": 10, "ok_24h": 7, "failing": 3, "productive": 2},
    "crawler": {"running": False, "last_run": 1_789_999_500, "passes": 4},
    "dht": {"mode": "polite", "running": True, "uptime_secs": 600, "virtual_nodes": 1, "external_ip": "public",
            "queries_per_sec": 50.0, "answered_share": 0.4, "hashes_per_min": 120.0, "queue_len": 5,
            "metadata_per_min": 3.5, "fetch_success_rate": 0.25, "stored_total": 300, "adult_total": 20,
            "blocked_total": 2, "upnp": {"state": "off", "mappings": []}},
}

VALUES = {
    "category": [("video", 600), ("audio", 250), ("book", 150)],
    "language": [("fr", 400), ("en", 300), ("ja", 100)],
    "resolution": [("1080p", 500), ("720p", 200), ("2160p", 100)],
    "media_kind": [("movie", 300), ("series", 200)],
    "origin": [("nostr", 700), ("dht", 300)],
    "year": [("2024", 300), ("2023", 200), ("2020", 50)],
}


def breakdown(req):
    q = req.get("query", {})
    total = 1000 // (2 if q.get("languages") else 1)
    out = []
    for f in req.get("fields", []):
        counts = [{"value": v, "count": c // (2 if q.get("languages") and f != "language" else 1)} for v, c in VALUES.get(f, [])]
        out.append({"field": f, "total": total, "counts": counts, "other": 0, "other_values": 0, "missing": 10})
    return {"total": total, "breakdowns": out, "computed_at": 1_790_000_000}


def graph(kind):
    nodes = [{"id": ROOT_HEX, "kind": "root", "label": "You", "weight": 0, "score": None, "detail": ["you (trust root)"]}]
    edges = []
    for i in range(1, 12):
        nodes.append({"id": "%064x" % i, "kind": "publisher", "label": "pub%d" % i, "weight": i, "score": 5 * i,
                      "detail": ["%d torrents" % i]})
        if i <= 8:
            edges.append({"source": 0, "target": i, "weight": 1, "kind": "follows"})
    return {"kind": kind, "nodes": nodes, "edges": edges, "truncated": False, "note": None}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def authed(self):
        if self.headers.get("Authorization") != "Bearer " + TOKEN:
            self.send(401, {"error": "unauthorized", "message": "missing or invalid token"})
            return False
        return True

    def route(self, method):
        path = self.path.split("?")[0]
        with State.lock:
            State.requests.append(method + " " + path)
        if path == "/health":
            return self.send(200, {"status": "ok", "version": "uitest", "db": True, "features": FEATURES})
        if path == "/_uitest/saved":
            with State.lock:
                return self.send(200, State.saved)
        if path == "/_uitest/requests":
            with State.lock:
                return self.send(200, State.requests)
        if path == "/_uitest/breakdowns":
            with State.lock:
                return self.send(200, State.breakdowns)
        if path == "/_uitest/trust_puts":
            with State.lock:
                return self.send(200, State.trust_puts)
        if path == "/_uitest/searches":
            with State.lock:
                return self.send(200, State.searches)
        if not self.authed():
            return
        if path == "/api/v1/search" and method == "POST":
            q = self.body()
            with State.lock:
                State.searches.append(q)
            hits = [h for h in HITS if q.get("text", "").lower() in h["title"].lower()]
            if q.get("resolutions"):
                hits = [h for h in hits if h["attrs"]["resolution"] in q["resolutions"]]
            if q.get("origins") == ["dht"]:
                hits = [h for h in hits if h["origin"] in ("dht", "both")]
            return self.send(200, {"hits": hits, "total": len(hits), "source": "server", "total_is_lower_bound": False})
        if path == "/api/v1/facets":
            return self.send(200, {"total": 3, "facets": [
                {"field": "category", "total": 3, "counts": [{"value": "video", "count": 3}]},
                {"field": "origin", "total": 3, "counts": [{"value": "nostr", "count": 2}, {"value": "dht", "count": 2}]},
                {"field": "resolution", "total": 3, "counts": [{"value": "1080p", "count": 3}]},
            ]})
        if path.startswith("/api/v1/torrents/") and path.endswith("/liveness-history"):
            return self.send(200, {"infohash": "", "points": [{"at": 1_789_000_000, "seeders": 5, "leechers": 1},
                                                               {"at": 1_789_900_000, "seeders": 9, "leechers": 2}],
                                   "keep_days": 90})
        if path.startswith("/api/v1/torrents/"):
            ih = path.split("/")[4]
            h = next((x for x in HITS if x["infohash"] == ih), None)
            if h is None:
                return self.send(404, {"error": "not_found", "message": "unknown torrent"})
            return self.send(200, {"summary": h, "description": "", "files": [{"path": "a/movie.mkv", "size": 1_500_000_000}],
                                   "trackers": [], "hashtags": [], "category_path": [], "external_ids": [],
                                   "magnet": "magnet:?xt=urn:btih:" + ih, "liveness": None,
                                   "trust": {"pubkey": "", "score": 40, "factors": [], "root": None, "computed_at": 0, "graph_version": 0},
                                   "events": [], "comments": [],
                                   "media": {"media_id": 42, "kind": "movie", "title": "Big Buck Bunny", "year": 2008,
                                             "genres": ["Animation"], "rating": 6.4, "method": "manual", "confidence": 1.0,
                                             "overview": "A giant rabbit."} if h.get("media_id") else None})
        if path.startswith("/api/v1/media/") and path.endswith("/releases"):
            return self.send(200, {"media": {"media_id": 42, "kind": "movie", "title": "Big Buck Bunny", "year": 2008,
                                             "method": "manual", "confidence": 1.0},
                                   "total": 1, "truncated": False,
                                   "groups": [{"season": None, "episode": None, "resolution": "1080p", "language": "vff",
                                               "language_label": "VFF (France)", "releases": [HITS[0]]}]})
        if path == "/api/v1/stats/health":
            return self.send(200, STATS_HEALTH)
        if path == "/api/v1/stats/breakdown" and method == "POST":
            req = self.body()
            with State.lock:
                State.breakdowns.append(req)
            return self.send(200, breakdown(req))
        if path == "/api/v1/stats/timeseries":
            qs = self.path.split("?", 1)[1] if "?" in self.path else ""
            params = dict(p.split("=", 1) for p in qs.split("&") if "=" in p)
            metric = params.get("metric", "torrents_total").split(",")[0]
            bucket = params.get("bucket", "day")
            step = 3600 if bucket == "hour" else 86400
            counter = metric not in ("torrents_total", "db_size")
            pts = [{"at": 1_789_000_000 + i * step, "value": (i * 7) % 40 if counter else 500 + i * 5} for i in range(30)]
            return self.send(200, {"since": 1_789_000_000, "until": 1_790_000_000,
                                   "series": [{"metric": metric, "kind": "counter" if counter else "gauge",
                                               "bucket": bucket, "points": pts}]})
        if path.startswith("/api/v1/graphs/"):
            return self.send(200, graph(path.split("/")[4]))
        if path == "/api/v1/settings/trust" and method == "GET":
            with State.lock:
                return self.send(200, State.trust)
        if path == "/api/v1/settings/trust" and method == "PUT":
            t = self.body()
            with State.lock:
                State.trust_puts.append(t)
                State.trust = {"root": t.get("root"), "threshold": t.get("threshold", 20)}
                return self.send(200, State.trust)
        if path == "/api/v1/saved-searches" and method == "GET":
            with State.lock:
                return self.send(200, State.saved)
        if path == "/api/v1/saved-searches" and method == "POST":
            s = self.body()
            with State.lock:
                s["id"] = State.next_id
                State.next_id += 1
                State.saved.append(s)
            return self.send(200, s)
        if path.startswith("/api/v1/saved-searches/") and method == "DELETE":
            sid = int(path.rsplit("/", 1)[1])
            with State.lock:
                State.saved = [s for s in State.saved if s["id"] != sid]
            self.send_response(204)
            self.end_headers()
            return
        if path.startswith("/api/v1/saved-searches/poll"):
            return self.send(200, {"watermark": 1_790_000_000, "results": []})
        return self.send(404, {"error": "not_found", "message": path})

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def do_PUT(self):
        self.route("PUT")

    def do_DELETE(self):
        self.route("DELETE")


def serve(port):
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


if __name__ == "__main__":
    serve(int(sys.argv[1]) if len(sys.argv) > 1 else 18787)
    threading.Event().wait()
