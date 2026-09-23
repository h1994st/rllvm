#!/usr/bin/env bash
# Verifies the Claude Code plugin in plugins/rllvm: the MCP server it declares
# starts and answers, its scripts behave, and its skills are well formed.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

REPO=$(cd "$(dirname "$0")/../.." && pwd)
PLUGIN=$REPO/plugins/rllvm
mkdir -p "$OUT"

# The server entry is read from the plugin's own .mcp.json, so this proves
# what ships rather than a hand-written copy of it.
python3 - "$PLUGIN/.mcp.json" "$OUT" <<'PY'
import json, subprocess, sys

server = json.load(open(sys.argv[1]))["mcpServers"]["rllvm-query"]
request = '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n'
reply = subprocess.run(
    [server["command"], *server["args"]],
    input=request, capture_output=True, text=True, check=True,
).stdout
names = {tool["name"] for tool in json.loads(reply)["result"]["tools"]}
for expected in ("load_catalog", "inventory"):
    if expected not in names:
        sys.exit(f"the plugin's server omitted {expected}; got {sorted(names)}")
PY
echo "ok: the plugin's MCP entry starts rllvm-query and lists its tools"

# CI does not install Claude Code, so strict validation runs where it is.
if command -v claude >/dev/null; then
    claude plugin validate --strict "$PLUGIN" >"$OUT/validate-plugin.txt" 2>&1 ||
        fail "plugin manifest: $(cat "$OUT/validate-plugin.txt")"
    claude plugin validate --strict "$REPO" >"$OUT/validate-marketplace.txt" 2>&1 ||
        fail "marketplace manifest: $(cat "$OUT/validate-marketplace.txt")"
    echo "ok: claude plugin validate --strict passes"
else
    echo "note: claude not installed; manifests not validated"
fi
