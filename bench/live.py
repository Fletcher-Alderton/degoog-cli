#!/usr/bin/env python3
"""End-to-end comparison against a real Degoog instance.

The main benchmark (bench.py) keeps the network out on purpose. This
one does the opposite: both implementations search a live Degoog server
and scrape real pages over the internet, so the numbers show how much
of the difference a user actually feels.

Usage:
    DEGOOG_URL=https://your.degoog DEGOOG_API_KEY=... bench/live.py

Queries alternate which implementation goes first, so neither one always
gets the server's warm cache. Numbers here depend on the server, the
network and the sites scraped; treat them as indicative.
"""

import json
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench  # noqa: E402

QUERIES = [
    "rust async runtime comparison",
    "postgres vacuum tuning",
    "how does http/3 work",
    "sqlite wal mode",
    "linux cgroups v2 memory limit",
    "react server components explained",
    "zig vs rust memory safety",
    "bm25 ranking function",
]


def main():
    url = os.environ.get("DEGOOG_URL", "").rstrip("/")
    key = os.environ.get("DEGOOG_API_KEY", "")
    if not url:
        sys.exit("set DEGOOG_URL (and DEGOOG_API_KEY if your instance needs one)")
    binary = os.path.join(bench.ROOT, "target", "release", "degoog")
    bun, main_js = bench.setup_upstream()

    cfg_dir = os.path.join(bench.WORK, "cfg-live")
    os.makedirs(cfg_dir, exist_ok=True)
    ours_cfg = os.path.join(cfg_dir, "degoog.toml")
    with open(ours_cfg, "w") as f:
        f.write(f'[degoog]\nurl = "{url}"\n[bundle_search]\nmax_scrape_urls = 3\n'
                "[cache]\nenabled = false\n")
    up_cfg = os.path.join(cfg_dir, "mcp.yml")
    with open(up_cfg, "w") as f:
        f.write(f'server:\n  host: "127.0.0.1"\n  port: {bench.UP_PORT}\n'
                f'degoog:\n  url: "{url}"\nbundleSearch:\n  maxScrapeUrls: 3\n'
                "cache:\n  enabled: false\n")
    base = {k: v for k, v in os.environ.items() if not k.startswith(("DEGOOG", "XDG_C"))}
    ours_env = dict(base, DEGOOG_CONFIG=ours_cfg, DEGOOG_API_KEY=key, DEGOOG_LOG_LEVEL="error",
                    XDG_CACHE_HOME=os.path.join(bench.WORK, "xdg-cache"))
    up_env = dict(base, DEGOOG_MCP_CONFIG=up_cfg, DEGOOG_MCP_DEGOOG_API_KEY=key,
                  DEGOOG_MCP_LOG_LEVEL="error", NODE_ENV="production")

    clients = {
        "degoog-cli": bench.StdioClient(binary, ours_env),
        "upstream": bench.HttpClient(bun, main_js, up_env),
    }
    for c in clients.values():
        c.ready()
    cpu0 = {n: bench.ps_sample(c.pid)[1] for n, c in clients.items()}

    times = {n: {"search": [], "bundle_search": []} for n in clients}
    sources = {n: [] for n in clients}
    for tool in ["search", "bundle_search"]:
        for i, q in enumerate(QUERIES):
            order = list(clients) if i % 2 == 0 else list(reversed(clients))
            for name in order:
                c = clients[name]
                t0 = time.perf_counter()
                msg = c.request("tools/call", {"name": tool, "arguments": {"query": q}})
                ms = (time.perf_counter() - t0) * 1000
                res = msg.get("result") or {}
                if res.get("isError"):
                    bench.log(f"  {name} {tool} {q!r}: {res['content'][0]['text'][:120]}")
                    continue
                times[name][tool].append(ms)
                if tool == "bundle_search":
                    sources[name].append(res["structuredContent"]["counts"]["sources"])
            bench.log(f"  {tool:14} {q[:30]:30} " + "  ".join(
                f"{n} {times[n][tool][-1]:7.0f} ms" for n in clients if times[n][tool]))

    report = {"server": "live Degoog instance", "queries": len(QUERIES), "results": {}}
    for name, c in clients.items():
        cpu = bench.ps_sample(c.pid)[1] - cpu0[name]
        rss = bench.ps_sample(c.pid)[0] / 1024
        c.close()
        calls = sum(len(v) for v in times[name].values())
        report["results"][name] = {
            tool: {"p50_ms": statistics.median(v), "mean_ms": statistics.mean(v), "n": len(v)}
            for tool, v in times[name].items() if v
        }
        report["results"][name]["cpu_ms_per_call"] = cpu * 1000 / max(calls, 1)
        report["results"][name]["rss_mb"] = rss
        report["results"][name]["bundle_sources_mean"] = statistics.mean(sources[name]) if sources[name] else 0

    out = os.path.join(bench.ROOT, "bench", "live-results.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
