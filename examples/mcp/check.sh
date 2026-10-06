#!/usr/bin/env bash
# Talks to the MCP server the way a client does: JSON-RPC over stdio, a full
# session per protocol era. mcp_session.py drives the conversation and checks
# the answers.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-cc rllvm-query python3

mkdir -p "$OUT"
# A captured object carries its own bitcode, so `inventory` can load it over
# the wire -- no separate catalog file to build.
rllvm-cc -g -O0 -c lib.c -o "$OUT/lib.o"

python3 "$(dirname "$0")/mcp_session.py" "$OUT"

echo "ok: legacy and modern sessions both loaded a module and answered defs over MCP"
