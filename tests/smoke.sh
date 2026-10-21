#!/usr/bin/env bash
# End-to-end check of the CLI and the stdio MCP server against a fake Degoog.
# Usage: tests/smoke.sh   (needs python3)
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q
PORT=4499
python3 tests/fixtures/mock_degoog.py $PORT & MOCK=$!
trap 'kill $MOCK 2>/dev/null' EXIT
sleep 1

CFG=$(mktemp)
printf '[degoog]\nurl = "http://127.0.0.1:%s"\napi_key = "sekrit"\n[scrape]\nallow_private_ips = true\n' $PORT > "$CFG"
D="./target/debug/degoog --config $CFG --no-cache"
fail() { echo "FAIL: $*"; exit 1; }

$D health | grep -q "reachable" || fail health
$D search tokio | grep -q '\[S1\] Tokio' || fail search
$D search -t news x | grep -q 'News item' || fail tab-search
$D scrape -q wakers http://127.0.0.1:$PORT/page/redirect http://127.0.0.1:$PORT/page/missing | grep -q 'HTTP 404' || fail scrape
$D bundle tokio --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["counts"]["sources"]>=1, d' || fail bundle
$D discover | grep -q '!weather' || fail discover
$D retry bing tokio | grep -q 'returned 7 results' || fail retry
$D command '!weather berlin' | grep -q 'Sunny' || fail command
{ ./target/debug/degoog --config "$CFG" --api-key wrong --no-cache search x 2>&1 || true; } | grep -q 'rejected the API key' || fail auth-hint

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bundle_search","arguments":{"query":"tokio"}}}' \
  | $D mcp | python3 -c '
import json,sys
msgs={m["id"]:m for m in map(json.loads,sys.stdin)}
assert msgs[1]["result"]["protocolVersion"]=="2025-06-18"
assert len(msgs[2]["result"]["tools"])==7
r=msgs[3]["result"]; assert not r["isError"] and r["structuredContent"]["evidence"], r
' || fail mcp
echo "smoke ok"
