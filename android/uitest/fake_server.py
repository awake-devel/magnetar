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
            "saved_search_poll", "browse_checked_dht"]


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
