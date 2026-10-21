#!/usr/bin/env python3
"""Benchmark degoog-cli against the upstream degoog MCP server.

Both run against the same fake Degoog backend on loopback, which serves a
canned search response and real HTML pages downloaded once. That keeps
the network out of the numbers, so what is measured is each
implementation's own work: JSON handling, dedupe, HTML extraction,
chunking and the MCP transport.

Usage: bench/bench.py [--quick]

Needs: a release build of this repo (cargo build --release), git, npm,
and python3 (stdlib only). Upstream and its bun runtime are fetched into
bench/.work on first run.
"""

import http.client
import http.server
import json
import os
import queue
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = os.path.join(ROOT, "bench", ".work")
UPSTREAM_REPO = "https://github.com/degoog-org/mcp"
UPSTREAM_REV = "6a8a8bb"
BUN_VERSION = "1.3.14"
API_PORT = 4499
UP_PORT = 4599
API_KEY = "bench-key"

PAGES = {
    "rust": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "tokio": "https://docs.rs/tokio/latest/tokio/",
    "fetch": "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch",
    "asyncio": "https://docs.python.org/3/library/asyncio-task.html",
    "http": "https://en.wikipedia.org/wiki/HTTP",
}
# Three loopback hosts, so the source picker sees three sites.
HOSTS = ["127.0.0.1", "localhost", "[::1]"]

QUICK = "--quick" in sys.argv
N = {"search": 200, "scrape": 40, "bundle_search": 40, "discover": 100}
if QUICK:
    N = {k: max(5, v // 10) for k, v in N.items()}
WARMUP = 5
CONCURRENCY = 16
CONCURRENT_ROUNDS = 1 if QUICK else 4
COLD_RUNS = 3 if QUICK else 10


def log(msg):
    print(msg, file=sys.stderr, flush=True)


# --------------------------------------------------------------- setup

def run(cmd, cwd=None):
    subprocess.run(cmd, cwd=cwd, check=True, stdout=subprocess.DEVNULL)


def fetch_pages():
    pages_dir = os.path.join(WORK, "pages")
    os.makedirs(pages_dir, exist_ok=True)
    out = {}
    for name, url in PAGES.items():
        path = os.path.join(pages_dir, name + ".html")
        if not os.path.exists(path):
            log(f"fetching {url}")
            req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 degoog-bench"})
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = resp.read()
            with open(path, "wb") as f:
                f.write(data)
        with open(path, "rb") as f:
            out[name] = f.read()
    return out


def setup_upstream():
    up = os.path.join(WORK, "upstream")
    bun_dir = os.path.join(WORK, "bun")
    bun = os.path.join(bun_dir, "node_modules", "bun", "bin", "bun.exe")
    if not os.path.exists(bun):
        log(f"installing bun {BUN_VERSION} into {bun_dir}")
        run(["npm", "install", "--silent", "--prefix", bun_dir, f"bun@{BUN_VERSION}"])
    if not os.path.isdir(up):
        log(f"cloning {UPSTREAM_REPO}")
        run(["git", "clone", "-q", UPSTREAM_REPO, up])
    run(["git", "checkout", "-q", UPSTREAM_REV], cwd=up)
    main_js = os.path.join(up, "dist", "main.js")
    if not os.path.exists(main_js):
        log("building upstream (bun run build)")
        run([bun, "install", "--frozen-lockfile"], cwd=up)
        run([bun, "run", "build"], cwd=up)
    return bun, main_js


# ------------------------------------------------------- fake backend

def search_results(query):
    names = list(PAGES)
    results = []
    for i in range(20):
        name = names[i % len(names)]
        host = HOSTS[i % len(HOSTS)]
        url = f"http://{host}:{API_PORT}/page/{name}?v={i // 6}"
        if i % 7 == 3:
            url += "&utm_source=bench"
        results.append({
            "title": f"{name.title()} <b>{query}</b> result {i}",
            "url": url,
            "snippet": f"About {query}: " + "lorem ipsum dolor sit amet " * 6,
            "source": ["google", "bing", "brave", "duckduckgo"][i % 4],
            "sources": ["google", "bing"] if i % 5 == 0 else [],
            "score": 20 - i,
        })
    return {
        "results": results,
        "query": query,
        "type": "web",
        "totalTime": 120,
        "engineTimings": [
            {"name": e, "time": 40 + j, "resultCount": 10}
            for j, e in enumerate(["google", "bing", "brave", "duckduckgo"])
        ],
        "relatedSearches": [f"{query} tutorial", f"{query} example"],
    }


class Backend(http.server.BaseHTTPRequestHandler):
    pages = {}
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send(self, code, body, ctype="application/json"):
        if not isinstance(body, bytes):
            body = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        from urllib.parse import parse_qs, urlparse
        u = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if u.path.startswith("/page/"):
            page = self.pages.get(u.path[6:])
            if page is None:
                return self.send(404, b"no", "text/html")
            return self.send(200, page, "text/html; charset=utf-8")
        if u.path == "/healthz":
            return self.send(200, {"ok": True})
        if self.headers.get("Authorization") != f"Bearer {API_KEY}":
            return self.send(401, {"error": "auth"})
        if u.path == "/api/search":
            return self.send(200, search_results(q.get("q", "")))
        if u.path == "/api/search-tabs":
            return self.send(200, {"tabs": [
                {"id": "engine:images", "name": "Images", "icon": None},
                {"id": "news", "name": "News", "icon": None},
            ]})
        if u.path == "/api/extensions":
            if q.get("type") == "engine":
                return self.send(200, {"engines": [
                    {"id": e, "name": e.title(), "enabled": True}
                    for e in ["google", "bing", "brave", "duckduckgo"]
                ]})
            return self.send(200, {"autocomplete": [{"id": "ddg"}]})
        if u.path == "/api/commands":
            return self.send(200, {"commands": [{"trigger": "weather"}, {"trigger": "w"}]})
        if u.path == "/api/suggest":
            return self.send(200, ["one", "two"])
        return self.send(404, {"error": "not found"})


class Quiet(http.server.ThreadingHTTPServer):
    """A client closing a keep-alive connection early is not news."""

    def handle_error(self, request, client_address):
        pass


class V6Server(Quiet):
    address_family = socket.AF_INET6


def start_backend(pages):
    Backend.pages = pages
    servers = [
        Quiet(("127.0.0.1", API_PORT), Backend),
        V6Server(("::1", API_PORT), Backend),
    ]
    for s in servers:
        s.daemon_threads = True
        threading.Thread(target=s.serve_forever, daemon=True).start()
    return servers


# ---------------------------------------------------- process sampling

def ps_sample(pid):
    """(rss_kb, cpu_seconds) of pid, or None once it is gone."""
    out = subprocess.run(["ps", "-o", "rss=,time=", "-p", str(pid)],
                         capture_output=True, text=True).stdout.split()
    if len(out) < 2:
        return None
    rss = int(out[0])
    t = out[1]
    parts = t.split(":")
    secs = float(parts[-1]) + 60 * float(parts[-2]) + (3600 * float(parts[-3]) if len(parts) > 2 else 0)
    return rss, secs


class Sampler:
    """Polls RSS every 20ms to catch the peak between ps calls."""

    def __init__(self, pid):
        self.pid = pid
        self.peak = 0
        self.stop_flag = False
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.thread.start()

    def loop(self):
        while not self.stop_flag:
            s = ps_sample(self.pid)
            if s:
                self.peak = max(self.peak, s[0])
            time.sleep(0.02)

    def stop(self):
        self.stop_flag = True
        self.thread.join()


# ------------------------------------------------------------ clients

INIT = {"protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "bench", "version": "0"}}


class StdioClient:
    """degoog-cli over MCP stdio."""

    name = "degoog-cli"

    def __init__(self, binary, env):
        self.proc = subprocess.Popen([binary, "--no-cache", "mcp"], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)
        self.pid = self.proc.pid
        self.next_id = 0
        self.lock = threading.Lock()
        self.waiting = {}
        threading.Thread(target=self.reader, daemon=True).start()

    def reader(self):
        for line in self.proc.stdout:
            msg = json.loads(line)
            q = self.waiting.pop(msg.get("id"), None)
            if q:
                q.put(msg)

    def request(self, method, params=None):
        with self.lock:
            self.next_id += 1
            rid = self.next_id
            q = queue.Queue()
            self.waiting[rid] = q
            self.proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": rid, "method": method,
                                               "params": params or {}}) + "\n").encode())
            self.proc.stdin.flush()
        return q.get(timeout=120)

    def notify(self, method):
        with self.lock:
            self.proc.stdin.write((json.dumps({"jsonrpc": "2.0", "method": method}) + "\n").encode())
            self.proc.stdin.flush()

    def ready(self):
        self.request("initialize", INIT)
        self.notify("notifications/initialized")

    def close(self):
        self.proc.stdin.close()
        _, status, ru = os.wait4(self.pid, 0)
        self.proc.returncode = status
        return ru


class HttpClient:
    """Upstream over MCP streamable HTTP, one session."""

    name = "upstream (bun)"

    def __init__(self, bun, main_js, env):
        self.proc = subprocess.Popen([bun, "run", main_js], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
        self.pid = self.proc.pid
        self.session = None
        self.next_id = 0
        self.lock = threading.Lock()

    def post(self, body):
        conn = http.client.HTTPConnection("127.0.0.1", UP_PORT, timeout=120)
        headers = {"content-type": "application/json",
                   "accept": "application/json, text/event-stream"}
        if self.session:
            headers["mcp-session-id"] = self.session
            headers["mcp-protocol-version"] = "2025-06-18"
        conn.request("POST", "/mcp", json.dumps(body), headers)
        resp = conn.getresponse()
        sid = resp.getheader("mcp-session-id")
        if sid:
            self.session = sid
        if "id" not in body:
            resp.read()
            conn.close()
            return None
        ctype = resp.getheader("content-type", "")
        if ctype.startswith("application/json"):
            msg = json.loads(resp.read())
            conn.close()
            return msg
        for raw in resp:
            line = raw.decode().strip()
            if line.startswith("data:"):
                msg = json.loads(line[5:])
                if msg.get("id") == body["id"]:
                    conn.close()
                    return msg
        conn.close()
        raise RuntimeError("no answer")

    def request(self, method, params=None):
        with self.lock:
            self.next_id += 1
            rid = self.next_id
        return self.post({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})

    def ready(self):
        deadline = time.time() + 30
        while True:
            try:
                self.request("initialize", INIT)
                break
            except (ConnectionRefusedError, OSError):
                if time.time() > deadline:
                    raise
                time.sleep(0.005)
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def close(self):
        self.proc.terminate()
        _, status, ru = os.wait4(self.pid, 0)
        self.proc.returncode = status
        return ru


# ----------------------------------------------------------- workload

ARGS = {
    "search": lambda i: {"query": f"rust async runtime {i % 10}"},
    "scrape": lambda i: {"urls": [f"http://127.0.0.1:{API_PORT}/page/{n}" for n in
                                  ["rust", "tokio", "fetch", "asyncio"]],
                         "query": "async runtime"},
    "bundle_search": lambda i: {"query": f"rust async runtime {i % 10}"},
    "discover": lambda i: {},
}


def call(client, tool, i):
    t0 = time.perf_counter()
    msg = client.request("tools/call", {"name": tool, "arguments": ARGS[tool](i)})
    ms = (time.perf_counter() - t0) * 1000
    result = msg.get("result")
    if not result or result.get("isError"):
        raise RuntimeError(f"{client.name} {tool} failed: {json.dumps(msg)[:400]}")
    return ms, result.get("structuredContent", {})


def shape(tool, sc):
    """What a result contained, to check both did the same work."""
    if tool == "search":
        return {"results": len(sc.get("results", []))}
    if tool == "scrape":
        return {"useful": sc["counts"]["useful"],
                "chunks": sum(len(s.get("chunks", [])) for s in sc.get("sources", []))}
    if tool == "bundle_search":
        return {"sources": sc["counts"]["sources"], "chunks": sc["counts"]["chunks"],
                "chars": sc["counts"]["evidenceChars"]}
    return {"engines": len(sc.get("engines", []))}


def pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def measure(make_client):
    out = {"tools": {}}
    client = make_client()
    t0 = time.perf_counter()
    client.ready()
    out["first_ready_ms"] = (time.perf_counter() - t0) * 1000
    time.sleep(1.0)
    rss, cpu = ps_sample(client.pid)
    out["idle_rss_mb"] = rss / 1024
    out["startup_cpu_s"] = cpu
    sampler = Sampler(client.pid)

    for tool in N:
        for i in range(WARMUP):
            call(client, tool, i)
    _, cpu_before = ps_sample(client.pid)

    for tool, n in N.items():
        lat = []
        c0 = ps_sample(client.pid)[1]
        w0 = time.perf_counter()
        for i in range(n):
            ms, sc = call(client, tool, i)
            lat.append(ms)
        wall = time.perf_counter() - w0
        c1 = ps_sample(client.pid)[1]
        out["tools"][tool] = {
            "n": n,
            "p50_ms": statistics.median(lat),
            "p95_ms": pct(lat, 0.95),
            "mean_ms": statistics.mean(lat),
            "cpu_ms_per_call": (c1 - c0) * 1000 / n,
            "calls_per_s": n / wall,
            "shape": shape(tool, sc),
        }
        log(f"  {client.name:15} {tool:14} p50 {out['tools'][tool]['p50_ms']:7.2f} ms  "
            f"cpu {out['tools'][tool]['cpu_ms_per_call']:6.2f} ms/call")

    out["rss_after_load_mb"] = ps_sample(client.pid)[0] / 1024

    # CONCURRENCY calls in flight at once: cheap searches, then
    # bundle_search, which parses pages and is CPU bound.
    out["concurrent"] = {}
    for tool, rounds in [("search", CONCURRENT_ROUNDS * 10), ("bundle_search", CONCURRENT_ROUNDS)]:
        c0 = ps_sample(client.pid)[1]
        w0 = time.perf_counter()
        total = 0
        lat = []
        for _ in range(rounds):
            threads, errors = [], []

            def one(i):
                try:
                    lat.append(call(client, tool, i)[0])
                except Exception as e:  # noqa: BLE001
                    errors.append(e)

            for i in range(CONCURRENCY):
                t = threading.Thread(target=one, args=(i,))
                t.start()
                threads.append(t)
            for t in threads:
                t.join()
            if errors:
                raise errors[0]
            total += CONCURRENCY
        wall = time.perf_counter() - w0
        c1 = ps_sample(client.pid)[1]
        out["concurrent"][tool] = {
            "in_flight": CONCURRENCY,
            "calls": total,
            "calls_per_s": total / wall,
            "p50_ms": statistics.median(lat),
            "cpu_ms_per_call": (c1 - c0) * 1000 / total,
        }
        log(f"  {client.name:15} {tool:14} x{CONCURRENCY} {total / wall:7.1f} calls/s")
    time.sleep(2.0)
    out["rss_settled_mb"] = ps_sample(client.pid)[0] / 1024
    sampler.stop()
    ru = client.close()
    # ru_maxrss is bytes on macOS, kilobytes on Linux; ps reports kB.
    maxrss_kb = ru.ru_maxrss / 1024 if sys.platform == "darwin" else ru.ru_maxrss
    out["peak_rss_mb"] = max(sampler.peak, maxrss_kb) / 1024
    out["total_cpu_s"] = ru.ru_utime + ru.ru_stime
    return out


def cold_start(make_client):
    times = []
    for _ in range(COLD_RUNS):
        t0 = time.perf_counter()
        client = make_client()
        client.ready()
        client.request("tools/list")
        times.append((time.perf_counter() - t0) * 1000)
        client.close()
    return {"p50_ms": statistics.median(times), "min_ms": min(times)}


def cli_oneshot(binary, env, argv):
    times, cpus, rss = [], [], []
    for _ in range(COLD_RUNS):
        t0 = time.perf_counter()
        p = subprocess.Popen([binary, "--no-cache", *argv], stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, env=env)
        _, status, ru = os.wait4(p.pid, 0)
        times.append((time.perf_counter() - t0) * 1000)
        if status != 0:
            raise RuntimeError(f"degoog {' '.join(argv)} exited {status}")
        cpus.append((ru.ru_utime + ru.ru_stime) * 1000)
        rss.append(ru.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024))
    return {"p50_ms": statistics.median(times), "cpu_ms": statistics.median(cpus),
            "max_rss_mb": max(rss)}


def dir_mb(path):
    total = 0
    for dirpath, _, files in os.walk(path):
        for f in files:
            fp = os.path.join(dirpath, f)
            if not os.path.islink(fp):
                total += os.path.getsize(fp)
    return total / 1e6


# ---------------------------------------------------------------- main

def main():
    os.makedirs(WORK, exist_ok=True)
    binary = os.path.join(ROOT, "target", "release", "degoog")
    if not os.path.exists(binary):
        sys.exit("build first: cargo build --release")
    bun, main_js = setup_upstream()
    pages = fetch_pages()
    start_backend(pages)

    cfg_dir = os.path.join(WORK, "cfg")
    os.makedirs(cfg_dir, exist_ok=True)
    ours_cfg = os.path.join(cfg_dir, "degoog.toml")
    with open(ours_cfg, "w") as f:
        f.write(f'[degoog]\nurl = "http://127.0.0.1:{API_PORT}"\napi_key = "{API_KEY}"\n'
                "[scrape]\nallow_private_ips = true\n[bundle_search]\nmax_scrape_urls = 3\n"
                "[cache]\nenabled = false\n")
    up_cfg = os.path.join(cfg_dir, "mcp.yml")
    with open(up_cfg, "w") as f:
        f.write(f'server:\n  host: "127.0.0.1"\n  port: {UP_PORT}\n'
                f'degoog:\n  url: "http://127.0.0.1:{API_PORT}"\n'
                "scrape:\n  allowPrivateIps: true\nbundleSearch:\n  maxScrapeUrls: 3\n"
                "cache:\n  enabled: false\n")

    base_env = {k: v for k, v in os.environ.items() if not k.startswith(("DEGOOG", "XDG_C"))}
    ours_env = dict(base_env, DEGOOG_CONFIG=ours_cfg, DEGOOG_LOG_LEVEL="error",
                    XDG_CACHE_HOME=os.path.join(WORK, "xdg-cache"))
    up_env = dict(base_env, DEGOOG_MCP_CONFIG=up_cfg, DEGOOG_MCP_DEGOOG_API_KEY=API_KEY,
                  DEGOOG_MCP_LOG_LEVEL="error", NODE_ENV="production")

    ours = lambda: StdioClient(binary, ours_env)  # noqa: E731
    upstream = lambda: HttpClient(bun, main_js, up_env)  # noqa: E731

    report = {
        "machine": {
            "cpu": subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"],
                                  capture_output=True, text=True).stdout.strip()
            if sys.platform == "darwin" else os.uname().machine,
            "os": f"{os.uname().sysname} {os.uname().release}",
            "quick": QUICK,
        },
        "upstream_rev": UPSTREAM_REV,
        "bun": BUN_VERSION,
        "footprint": {
            "degoog-cli binary MB": os.path.getsize(binary) / 1e6,
            "upstream dist/main.js MB": os.path.getsize(main_js) / 1e6,
            "bun runtime MB": os.path.getsize(bun) / 1e6,
            "upstream node_modules MB": dir_mb(os.path.join(os.path.dirname(main_js), "..", "node_modules")),
        },
    }
    for name, make in [("degoog-cli", ours), ("upstream", upstream)]:
        log(f"cold start: {name}")
        report.setdefault("cold_start", {})[name] = cold_start(make)
        log(f"load: {name}")
        report.setdefault("load", {})[name] = measure(make)
    log("cli one-shot")
    report["cli_oneshot"] = {
        "search": cli_oneshot(binary, ours_env, ["search", "rust async runtime"]),
        "bundle": cli_oneshot(binary, ours_env, ["bundle", "rust async runtime", "-n", "3"]),
    }

    out = os.path.join(ROOT, "bench", "results.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    log(f"wrote {out}")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
