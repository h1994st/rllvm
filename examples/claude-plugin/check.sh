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
    # --json instead of --strict: the plugin ships without a version by
    # design (Claude Code tracks the git commit), so the missing-version
    # warning is the only one tolerated here; anything else still fails.
    claude plugin validate --json "$PLUGIN" >"$OUT/validate-plugin.json" || true
    python3 - "$OUT/validate-plugin.json" <<'PY'
import json, sys

report = json.load(open(sys.argv[1]))
sections = [report["manifest"], *report.get("contents", [])]
errors = [e for s in sections for e in s.get("errors", [])]
warnings = [
    w for s in sections for w in s.get("warnings", []) if w.get("path") != "version"
]
if errors or warnings:
    sys.exit(f"plugin manifest: errors={errors} warnings={warnings}")
PY
    # --json here too: --strict alone would fail on the same tolerated
    # missing-version warning, propagated through the plugin it lists.
    claude plugin validate --json "$REPO" >"$OUT/validate-marketplace.json" || true
    python3 - "$OUT/validate-marketplace.json" <<'PY'
import json, sys

report = json.load(open(sys.argv[1]))
sections = [report["manifest"], *report.get("contents", [])]
errors = [e for s in sections for e in s.get("errors", [])]
warnings = [
    w
    for s in sections
    for w in s.get("warnings", [])
    if not w.get("path", "").endswith("version")
]
if errors or warnings:
    sys.exit(f"marketplace manifest: errors={errors} warnings={warnings}")
PY
    echo "ok: claude plugin validate passes"
else
    echo "note: claude not installed; manifests not validated"
fi
