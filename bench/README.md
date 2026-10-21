# Benchmarks

Two scripts, both plain Python 3 with no packages to install.

`bench.py` runs degoog-cli and the upstream MCP server against the same fake Degoog backend on localhost and writes `results.json`. This is the comparison in the main README.

`live.py` runs both against a real Degoog instance and writes `live-results.json`. It needs `DEGOOG_URL` and, if your instance uses one, `DEGOOG_API_KEY`.

```sh
cargo build --release
python3 bench/bench.py            # about four minutes
python3 bench/bench.py --quick    # a tenth of the calls, for checking the setup
DEGOOG_URL=https://your.degoog DEGOOG_API_KEY=... python3 bench/live.py
```

On first run `bench.py` clones upstream at a fixed commit into `bench/.work/upstream`, installs bun with npm into `bench/.work/bun`, builds upstream with `bun run build`, and saves the five test pages into `bench/.work/pages`. Nothing is installed globally. `bench/.work` is in `.gitignore`.

## What is measured

The fake backend answers `/api/search` with 20 results spread over three loopback hosts (`127.0.0.1`, `localhost` and `[::1]`) so the source picker sees three different sites, the same way it would see three domains on a real server. The pages are real HTML: the Wikipedia articles on Rust and HTTP, the docs.rs page for tokio, MDN's fetch guide and the asyncio tasks page from the Python docs.

Each implementation is started fresh, given five warm-up calls of every tool, and then sent:

| Tool | Calls | Arguments |
| --- | ---: | --- |
| `search` | 200 | a query, 10 variants |
| `scrape` | 40 | four of the saved pages, with a query |
| `bundle_search` | 40 | a query; three sources |
| `discover` | 100 | none |

After that, two concurrent phases with 16 calls in flight: 640 `search` calls and 64 `bundle_search` calls.

Latency is wall time from sending the request to reading the answer, measured by the client. CPU is the server process's user plus system time, read from `ps` before and after each phase and divided by the number of calls; `ps` reports in hundredths of a second, which is why the phases are long. Total CPU and peak memory come from `wait4` when the process exits. Memory during the run is sampled from `ps` every 20 ms.

Cold start is the time from spawning the process to an answered `tools/list`, ten times each. For upstream that includes waiting for its HTTP port to open.

Both sides run with caching off, private addresses allowed (the backend is on loopback) and `max_scrape_urls` set to 3. Before trusting any timing, the script records what each call returned. In every run both sides returned the same number of results, read the same pages, and produced the same 12 chunks and 6,160 characters of evidence.

## Transport

degoog-cli is driven over stdio, which is how an MCP client runs it. Upstream is driven over streamable HTTP, which is the only transport it offers, with a new connection for each request. Opening a loopback connection costs well under a millisecond, which is visible only in the sub-millisecond `search` and `discover` rows. It makes no difference to the scraping rows.

## Results

Apple M3 Pro, 11 cores, 18 GB, macOS 26.6. Upstream at `6a8a8bb` on bun 1.3.14. Two full runs agreed within a few percent; `results.json` is the second.

| | degoog-cli | upstream |
| --- | ---: | ---: |
| `search` p50 / p95 | 0.33 / 0.40 ms | 0.90 / 1.73 ms |
| `scrape` p50 / p95 | 19.9 / 21.7 ms | 143 / 152 ms |
| `bundle_search` p50 / p95 | 20.1 / 20.5 ms | 115 / 121 ms |
| `discover` p50 / p95 | 0.36 / 0.41 ms | 0.53 / 1.15 ms |
| CPU per `scrape` | 32 ms | 167 ms |
| CPU per `bundle_search` | 24 ms | 122 ms |
| 16 concurrent `search` | 5,170 calls/s | 1,800 calls/s |
| 16 concurrent `bundle_search` | 210 calls/s | 8.6 calls/s |
| Median latency, 16 concurrent bundles | 54 ms | 1,011 ms |
| Total CPU for the whole run | 5.3 s | 23.1 s |
| Memory idle | 6 MB | 69 MB |
| Memory after the sequential phases | 38 MB | 446 MB |
| Memory after the concurrent phases | 189 MB | 533 MB |
| Cold start to `tools/list` | 3.4 ms | 70 ms |

The CPU per scrape is higher than the latency for degoog-cli because the four pages are parsed on four threads at once.

The memory after the concurrent phases is the high-water mark of 48 pages being parsed together, held by the allocator for reuse. A separate run of twelve rounds of 16 concurrent bundles each showed degoog-cli at 116, 141, 151, then flat at 152 MB, and upstream climbing from 457 to 557 MB without settling.

## Live run

Eight queries each for `search` and `bundle_search` against a real instance over a home connection, alternating which side asked first:

| | degoog-cli | upstream |
| --- | ---: | ---: |
| `search` median | 978 ms | 1,010 ms |
| `bundle_search` median | 3,030 ms | 2,424 ms |
| CPU per call | 55 ms | 321 ms |
| Memory at the end | 41 MB | 268 MB |

Single queries swung by seconds in both directions. Whoever asked first paid for Degoog's uncached search, about 1.3 s against 0.5 s for the second asker, and bundle times are dominated by the slowest of three third-party sites. With the search cache warmed beforehand, three repeated bundle queries chose identical pages on both sides and finished within a few hundred milliseconds of each other, with neither side consistently ahead. One site (medium.com) refused upstream's fetch with a 403 and served degoog-cli's, so on that query degoog-cli read a page upstream skipped.

The useful reading of this run is the last two rows. Over a real network a person will not notice a speed difference; the machine will notice the CPU and memory.

## Limits

- One machine and one OS. The ratios should hold elsewhere; the absolute numbers will not.
- The fake backend is a Python thread pool. It answers in well under a millisecond, but it is shared by both sides and sits under the concurrent numbers.
- The five pages are typical article pages. Very large or script-heavy pages will shift the absolute numbers.
- Upstream's HTTP transport and degoog-cli's stdio transport are not the same thing. See above for why that does not explain the gap.
