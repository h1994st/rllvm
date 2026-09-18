#!/usr/bin/env bash
# Talks to the MCP server the way a client does: JSON-RPC over stdio.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

mkdir -p "$OUT"
rllvm-cc -g -O0 -c lib.c -o "$OUT/lib.o"
rllvm-info "$OUT/lib.o" --json >"$OUT/catalog.json"

# One newline-delimited request in, one response out. A real client keeps the
# session open; tests/query.rs covers the session semantics, and this shows a
# client what the conversation looks like.
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' |
    rllvm-query mcp >"$OUT/response.json" 2>"$OUT/stderr.txt"

python3 - "$OUT/response.json" <<'PY'
import json, sys

lines = [line for line in open(sys.argv[1]) if line.strip()]

# Every line must parse. The server's contract is that stdout carries
# protocol frames and nothing else -- a stray log line or banner would
# corrupt the stream for any client reading it.
frames = []
for line in lines:
    try:
        frames.append(json.loads(line))
    except json.JSONDecodeError:
        sys.exit(f"stdout carried a non-protocol line: {line!r}")

if len(frames) != 1:
    sys.exit(f"expected one response frame, got {len(frames)}")

tools = frames[0].get("result", {}).get("tools")
if not tools:
    sys.exit(f"tools/list returned no tools: {frames[0]}")

names = {tool["name"] for tool in tools}
for expected in ("load_catalog", "defs"):
    if expected not in names:
        sys.exit(f"tools/list omitted {expected}; got {sorted(names)}")

print(f"  {len(tools)} tools offered")
PY

echo "ok: tools/list answered with protocol frames only on stdout"
