"""Tiny fake Degoog for smoke tests. Also serves pages to scrape under /page/."""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 4499
BASE = f"http://127.0.0.1:{PORT}"
ARTICLE = """<html><head><title>{t}</title></head><body><nav>Menu Home About</nav><article>
<h2>{t} details</h2><p>{t} is a page about rust async runtimes and the tokio scheduler, written with enough words to be a body.</p>
<p>A second paragraph on rust futures, wakers and executors, again long enough to survive the chunk minimum length check.</p>
</article><footer>footer text</footer></body></html>"""

def results(q):
    return [
        {"title": f"<b>Tokio</b> {q}", "url": f"{BASE}/page/one?utm_source=x", "snippet": "Tokio is a runtime", "source": "google", "score": 3},
        {"title": "Tokio dup", "url": f"{BASE}/page/one", "snippet": "dup", "source": "bing", "score": 2},
        {"title": "Broken", "url": f"{BASE}/page/missing", "snippet": "404s", "source": "bing"},
        {"title": "App shell", "url": f"http://localhost:{PORT}/page/shell", "snippet": "js only", "source": "brave"},
        {"title": "Async book", "url": f"http://[::1]:{PORT}/page/two", "snippet": "the async book", "source": "brave"},
    ]

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else (json.dumps(body) if ctype == "application/json" else body).encode()
        self.send_response(code); self.send_header("Content-Type", ctype); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def auth_ok(self):
        return self.headers.get("Authorization") == "Bearer sekrit"
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        if not self.auth_ok(): return self.send(401, {"error": "no"})
        if self.path.startswith("/api/search"):
            return self.send(200, {"results": results(body.get("query"))[:1], "query": body.get("query"), "type": "web", "totalTime": 5, "engineTimings": [{"name": e, "time": 1, "resultCount": 1} for e in body.get("engines", [])], "relatedSearches": []})
        self.send(404, {})
    def do_GET(self):
        u = urlparse(self.path); q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if u.path == "/healthz": return self.send(200, {"ok": True})
        if u.path.startswith("/page/"):
            name = u.path[6:]
            if name == "missing": return self.send(404, "nope", "text/html")
            if name == "shell": return self.send(200, '<html><body><div id="root"></div><script>window.__NEXT_DATA__={}</script></body></html>', "text/html")
            if name == "redirect": self.send_response(302); self.send_header("Location", "/page/one"); self.send_header("Content-Length", "0"); self.end_headers(); return
            if name == "image": return self.send(200, b"\x89PNG", "image/png")
            return self.send(200, ARTICLE.format(t=name.title()), "text/html; charset=utf-8")
        if not self.auth_ok(): return self.send(401, {"error": "no"})
        if u.path == "/api/search":
            return self.send(200, {"results": results(q.get("q")), "query": q.get("q"), "type": q.get("type", "web"), "totalTime": 12,
                "engineTimings": [{"name": "google", "time": 5, "resultCount": 1}], "relatedSearches": ["tokio tutorial", "async rust"]})
        if u.path == "/api/search/retry":
            return self.send(200, {"results": results(q.get("q"))[:2], "query": q.get("q"), "type": "web", "totalTime": 3,
                "engineTimings": [{"name": "Bing", "time": 3, "resultCount": 7, "status": "ok"}], "relatedSearches": []})
        if u.path == "/api/search-tabs":
            return self.send(200, {"tabs": [{"id": "engine:images", "name": "Images", "icon": None}, {"id": "news", "name": "News", "icon": None}]})
        if u.path == "/api/tab-search":
            return self.send(200, {"results": [{"title": "News item", "url": "https://news.example.com/a", "snippet": "n", "source": "news"}], "query": q.get("q"), "type": "news", "totalTime": 1, "engineTimings": [], "relatedSearches": []})
        if u.path == "/api/suggest": return self.send(200, ["tokio runtime", {"text": "tokio select"}])
        if u.path == "/api/extensions":
            if q.get("type") == "engine": return self.send(200, {"engines": [{"id": "google", "name": "Google", "enabled": True}, {"id": "bing", "name": "Bing", "enabled": False}]})
            return self.send(200, {"autocomplete": [{"id": "ddg"}]})
        if u.path == "/api/commands": return self.send(200, {"commands": [{"trigger": "weather", "description": "Weather forecast"}]})
        if u.path == "/api/command":
            return self.send(200, {"type": "html", "trigger": "weather", "title": "Weather", "html": "<div><h3>Berlin</h3><p>Sunny, 21C</p><script>x()</script></div>"})
        self.send(404, {"error": "not found"})

ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
