# degoog-cli

A small Rust client for a [Degoog](https://github.com/degoog-org/degoog) search server. It runs on your own machine, talks to your Degoog instance over HTTP, and gives you the same search, scrape and evidence tools two ways:

- `degoog search ...` and friends, for use in a terminal
- `degoog mcp`, a stdio MCP server for Claude Code, kage, Cursor or any other MCP client

It is a rewrite of [degoog-org/mcp](https://github.com/degoog-org/mcp), the official MCP sidecar, without the deep search feature.

## Why

The official MCP server is built to run next to Degoog as a sidecar container. It listens on a port, needs its own auth token, and does all page scraping from the server. That works well for a shared deployment, but for one person with one laptop it means running and securing another service on the server, and it puts every page fetch on the server's IP.

I wanted the opposite arrangement. Degoog stays on the server and does the searching. Everything after that (reading pages, pulling the article text out, cutting it into chunks, building the evidence pack) happens locally, in a process the MCP client starts and stops on its own. No port, no container, no second token.

Deep search was left out on purpose. Upstream implements it by calling an LLM provider from inside the MCP server. When the MCP client is itself a coding agent with a model attached, the client can run that loop better than the server can, so the feature added configuration without adding much.

Doing this in Rust also made it a lot cheaper to run, which matters for something that sits in the background of every agent session. The numbers are below.

## Install

```sh
cargo install --git https://github.com/Fletcher-Alderton/degoog-cli
degoog config init --url https://degoog.example.com
export DEGOOG_API_KEY=...   # only if your instance requires a key
degoog health
```

`config init` writes a commented `~/.config/degoog/config.toml` with mode 0600.

## Use from a terminal

```sh
degoog search rust async runtime
degoog search -t news -n 5 openai
degoog search tokio -e google -e brave
degoog scrape https://tokio.rs -q scheduler
degoog bundle how does the tokio scheduler work
degoog bundle tokio -Q "tokio vs async-std" --expand
degoog discover
degoog retry bing rust async
degoog command '!weather berlin'
```

`bundle` is the one to reach for first: it searches, reads the best few pages in Degoog's order, and prints the relevant parts of each with `[S1]` style labels.

Add `--json` to any command to get the structured result instead of text. Errors exit non-zero, and so does `health` when Degoog is unreachable. Logs go to stderr; `-v` and `-vv` add detail, `--quiet` removes it.

## Use as an MCP server

Claude Code:

```sh
claude mcp add degoog -e DEGOOG_API_KEY=... -- degoog mcp
```

Any client that takes a JSON config:

```json
{
  "mcpServers": {
    "degoog": {
      "command": "degoog",
      "args": ["mcp"],
      "env": { "DEGOOG_URL": "https://degoog.example.com" }
    }
  }
}
```

The tools are `search`, `scrape`, `bundle_search`, `discover`, `retry_engine`, `command` and `health`. Names, arguments and the shape of `structuredContent` match upstream, so prompts written for the official server keep working. Calls run concurrently and `notifications/cancelled` stops the work in flight.

## Configuration

Settings are read in this order, later ones winning: built-in defaults, the config file, environment variables, command-line flags.

| Environment | Flag | Meaning |
| --- | --- | --- |
| `DEGOOG_URL` | `--url` | Degoog base URL. Default `http://localhost:4444`. |
| `DEGOOG_API_KEY` | `--api-key` | Sent as `Authorization: Bearer`. Prefer the variable; flags show up in `ps`. |
| `DEGOOG_CONFIG` | `--config` | Config file. Default `~/.config/degoog/config.toml`. |
| `DEGOOG_LOG_LEVEL` | `-v`, `--quiet` | `debug`, `info`, `warn` (default) or `error`. |
| `DEGOOG_USER_AGENT` | | User agent for page fetches. |

The file uses the same options as upstream's `mcp.yml`, renamed to snake_case TOML: `[search]`, `[scrape]` (including `[scrape.fetcher]` for handing fetches to something like FlareSolverr), `[bundle_search]` (including `query_expansion`), `[output]` and `[cache]`. Any string can use `${VAR}` or `${VAR:-default}`. A value that does not parse falls back to its default with a warning rather than stopping the program. `degoog config show` prints the result with the key masked.

## How it works

A `bundle_search` call goes through these steps:

1. Ask Degoog for results (`/api/search`, or `/api/tab-search` for plugin tabs).
2. Normalise URLs, strip tracking parameters, merge duplicates and keep at most two results per site, all in Degoog's ranking order.
3. Pick one candidate per site and fetch the first wave of pages concurrently. Every redirect hop is checked against a private-address guard before it is followed.
4. Parse each page with html5ever, find the main content block, drop navigation and boilerplate, and convert what is left to plain Markdown.
5. Split the text into chunks under their headings and score them against the query.
6. If too many pages failed, fetch the next candidates in a second wave.
7. Pack the best chunks under a character budget with stable `[S1]` ids.

The HTML side replaces cheerio with the `scraper` crate. The MCP side is a small JSON-RPC loop over stdin and stdout rather than an SDK. Parsing runs on Tokio's blocking pool, so several pages are processed in parallel instead of one after another.

Search results and recent scrape failures are cached in `~/.cache/degoog/cache.json`, so separate terminal runs share them. `--no-cache` turns that off.

## Benchmarks

Both implementations were run against the same fake Degoog server on localhost. It returns a fixed 20-result search response and serves five real pages saved from Wikipedia, MDN, docs.rs and the Python docs. Keeping the network out isolates each program's own work. Both were checked to return identical output: the same results, the same pages read, the same 12 evidence chunks and the same 6,160 characters of evidence.

Upstream was built with `bun run build` and run with `bun run dist/main.js`, the same way its Dockerfile runs it, on bun 1.3.14 at commit `6a8a8bb`. Caching was off on both sides. Machine: Apple M3 Pro, 11 cores, 18 GB, macOS 26.6.

### Latency and CPU, one call at a time

| Tool | degoog-cli p50 | upstream p50 | degoog-cli CPU per call | upstream CPU per call |
| --- | ---: | ---: | ---: | ---: |
| `search` | 0.33 ms | 0.90 ms | 0.25 ms | 1.05 ms |
| `scrape` (4 pages) | 19.9 ms | 143 ms | 32 ms | 167 ms |
| `bundle_search` (3 pages) | 20.1 ms | 115 ms | 24 ms | 122 ms |
| `discover` | 0.36 ms | 0.53 ms | 0.3 ms | 0.5 ms |

The difference is almost all HTML work. On the calls that only shuffle JSON, both are under a millisecond.

### Throughput with 16 calls in flight

| Tool | degoog-cli | upstream |
| --- | ---: | ---: |
| `search` | 5,170 calls/s | 1,800 calls/s |
| `bundle_search` | 210 calls/s | 8.6 calls/s |

Upstream runs on a single JavaScript thread, so sixteen bundles that each need about 120 ms of parsing queue behind each other, and the median call took about a second. degoog-cli parses on all cores and the median stayed around 54 ms.

### Memory

| | degoog-cli | upstream |
| --- | ---: | ---: |
| Idle, after start | 6 MB | 69 MB |
| After 280 sequential calls | 38 MB | 446 MB |
| After the concurrent tests | 150 to 190 MB | 530 to 560 MB |

The last row is a high-water mark, not a leak. Sixteen bundles at once means up to 48 pages parsed at the same time, and the allocator keeps that memory for reuse. Repeating that load twelve times, degoog-cli levels off at about 152 MB. Upstream was still slowly rising at 557 MB.

### Startup and size

| | degoog-cli | upstream |
| --- | ---: | ---: |
| Process start to answered `tools/list` | 3.4 ms | 70 to 84 ms |
| What you install | one 7 MB binary | 63 MB bun runtime plus a 2.1 MB bundle |

As a one-shot terminal command, `degoog search` takes 6 ms of wall time and 7.5 MB of memory, and `degoog bundle` takes 29 ms and 21 MB, including process start. Upstream has no equivalent.

### Against a real server

The same comparison against a live Degoog instance over the internet, eight queries each, alternating which side went first:

| | degoog-cli | upstream |
| --- | ---: | ---: |
| `search` median | 0.98 s | 1.01 s |
| `bundle_search` median | 3.0 s | 2.4 s |
| CPU per call | 55 ms | 320 ms |
| Memory at the end | 41 MB | 268 MB |

Here the wall times are set by the network: Degoog's own search time, its cache, and how quickly each scraped site answers. The run-to-run spread on a single query was larger than the difference between the two. With Degoog's cache warmed first, both picked the same pages and finished within a few hundred milliseconds of each other. The saving you get in practice is CPU and memory, not seconds.

The full method, the raw numbers and the scripts are in [`bench/`](bench/). Run `python3 bench/bench.py` after `cargo build --release` to reproduce them.

## Differences from upstream

- No HTTP server, inbound token, Docker image or `deep_search`. The `server` and `deepSearch` config sections are gone.
- Pages are fetched from your machine. The private-address guard is still on by default, so a crafted search result cannot make the scraper hit your router or other LAN services. Set `scrape.allow_private_ips = true` if you need internal hosts.
- The cache can persist between runs.
- In a terminal, output shows full results and evidence without the guidance lines written for models. Over MCP, output follows `output.mode` and `text_mode` like upstream.

## Development

```sh
cargo test           # unit tests
tests/smoke.sh       # CLI and MCP end to end against a fake Degoog (needs python3)
python3 bench/bench.py
```

## License

AGPL-3.0, the same as upstream. This is a derivative of degoog-org/mcp.
