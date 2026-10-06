#!/usr/bin/env bash
# Completes a call graph with an agent-recorded overlay edge, and checks that
# the edge is walked only on request and never reported as proof.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3 llvm:llvm-nm

mkdir -p "$OUT"
rllvm-cc -g -O0 -c ops.c -o "$OUT/ops.o"
rllvm-cc -g -O0 -c app.c -o "$OUT/app.o"
rllvm-cc "$OUT/ops.o" "$OUT/app.o" -o "$OUT/app"
rllvm-info "$OUT/app" --json >"$OUT/catalog.json"
OVERLAY=$OUT/catalog.overlay.jsonl
rm -f "$OVERLAY"

query() {
    rllvm-query --catalog "$OUT/catalog.json" "$@"
}

# One field per question, so a wrong answer names the question that gave it.
field() {
    python3 -c '
import json, sys
answer = json.load(sys.stdin)
print(eval(sys.argv[1], {"a": answer, "json": json}))
' "$1"
}

# 1. The unresolved call through `ops@8` has one candidate: `handler`.
candidates=$(query resolution-candidates --json | field '[
    [c["function"]["symbol"] for c in g["candidates"]]
    for g in a["results"] if g.get("field") == {"record": "ops", "offset": 8}
]')
[ "$candidates" = "[['handler']]" ] ||
    fail "resolution-candidates for ops@8 gave $candidates, expected [['handler']]"

# 2. Over resolved edges alone, main does not reach handler.
path=$(query reach main handler --json | field 'a["results"]')
[ "$path" = None ] || fail "reach main handler found $path before any overlay edge"

# 3. An agent records what it read in the code.
echo '{"op":"add","via_field":{"record":"ops","offset":8},"to":"handler","confidence":"high","provenance":["ops.c:8: o->on_event = handler"]}' |
    query overlay record >"$OUT/record.txt"
defines "$(cat "$OUT/record.txt")" '^recorded 1, saved 1 ' \
    "overlay record did not save the edge: $(cat "$OUT/record.txt")"

# 4. Asked for, the edge completes the path, labeled as an agent step.
steps=$(query reach main handler --include-overlay --json |
    field 'a["uncertainty"]["agent_path_steps"]')
[ "$steps" = 1 ] || fail "reach --include-overlay used $steps agent steps, expected 1"
grounds=$(query reach main handler --include-overlay --json |
    field '[s["provenance"] for s in a["results"] if s["kind"] == "agent"]')
[ "$grounds" = "[['ops.c:8: o->on_event = handler']]" ] ||
    fail "the agent step carried provenance $grounds, expected the recorded one"
text=$(query reach main handler --include-overlay)
defines "$text" '^agent +dispatch -> handler at .*ops\.c:10 via ops@8 \[high, unverified\]$' \
    "the agent step was not labeled: $text"
defines "$text" '^    because ops\.c:8: o->on_event = handler$' \
    "the agent step did not say why it was claimed: $text"
defines "$text" '^not proven: this path uses 1 agent edge\(s\)$' \
    "the answer did not say it is not proven: $text"
# Not asked for, the overlay is not walked.
path=$(query reach main handler --json | field 'a["results"]')
[ "$path" = None ] || fail "reach without --include-overlay walked the overlay: $path"

# The slice through the edge is main, dispatch and handler, emitted as one
# module that defines them and nothing else.
rm -f "$OUT/slice.bc"
text=$(query slice main handler --include-overlay --emit-module "$OUT/slice.bc")
defines "$text" '^dispatch -> handler \(agent\)$' "the slice did not label its agent edge: $text"
defines "$text" '^not proven: this slice uses 1 agent edge\(s\)$' \
    "the slice did not say it is not proven: $text"
defines "$text" '^wrote .*slice\.bc: 3 functions from 2 modules$' \
    "the slice module was not written: $text"
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/slice.bc")
for present in main dispatch handler; do
    defines "$symbols" " [Tt] _?$present\$" "the slice module does not define $present: $symbols"
done
if grep -qE ' [Tt] _?install$' <<<"$symbols"; then
    fail "the slice module defines install, which is off the path: $symbols"
fi

# 5. Retracted, the edge is gone from every walk.
query overlay list --json | field 'json.dumps({
    "op": "retract", "edge": a["edges"][0]["key"], "reason": "re-read: not taken"
})' >"$OUT/retract.jsonl"
query overlay record <"$OUT/retract.jsonl" >/dev/null
path=$(query reach main handler --include-overlay --json | field 'a["results"]')
[ "$path" = None ] || fail "reach still used a retracted edge: $path"

# 6. Compaction drops the history: only the header is left.
query overlay compact >/dev/null
lines=$(wc -l <"$OVERLAY" | tr -d ' ')
[ "$lines" = 1 ] || fail "compaction left $lines lines, expected 1"

echo "ok: an overlay edge completes reach main handler only when asked, labeled not proven, and retracts cleanly"

# 7. The same loop over MCP, in one server process: record, walk before
#    saving, then save. Every stdout line must be a protocol frame.
python3 - "$OUT/catalog.json" <<'PY'
import json, subprocess, sys

catalog = sys.argv[1]
meta = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {},
}
calls = [
    ("load_catalog", {"path": catalog}),
    ("record_edges", {"records": [{
        "op": "add", "via_field": {"record": "ops", "offset": 8}, "to": "handler",
        "confidence": "high", "provenance": ["ops.c:8: o->on_event = handler"],
    }]}),
    ("reach", {"from": "main", "to": "handler", "include_overlay": True}),
    ("save_overlay", {}),
]
stdin = "".join(
    json.dumps({"jsonrpc": "2.0", "id": i, "method": "tools/call",
                "params": {"_meta": meta, "name": name, "arguments": arguments}}) + "\n"
    for i, (name, arguments) in enumerate(calls)
)
out = subprocess.run(["rllvm-query", "mcp"], input=stdin, capture_output=True,
                     text=True, check=True).stdout
answers = {}
for line in filter(str.strip, out.splitlines()):
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        sys.exit(f"stdout carried a non-protocol line: {line!r}")
    if "error" in frame:
        sys.exit(f"{calls[frame['id']][0]} was a protocol error: {frame['error']}")
    result = frame["result"]
    if result.get("isError"):
        sys.exit(f"{calls[frame['id']][0]} failed: {result['content'][0]['text']}")
    answers[calls[frame["id"]][0]] = json.loads(result["content"][0]["text"])
steps = answers["reach"]["uncertainty"].get("agent_path_steps")
if steps != 1:
    sys.exit(f"reach over MCP used {steps} agent steps, expected 1")
agent = [s for s in answers["reach"]["results"] if s["kind"] == "agent"]
if [s["provenance"] for s in agent] != [["ops.c:8: o->on_event = handler"]]:
    sys.exit(f"the agent step did not carry the recorded provenance: {agent}")
if answers["save_overlay"]["saved"] != 1:
    sys.exit(f"save_overlay wrote {answers['save_overlay']}, expected 1 record")
PY
lines=$(wc -l <"$OVERLAY" | tr -d ' ')
[ "$lines" = 2 ] || fail "save_overlay left $lines lines, expected the header and one add"

echo "ok: over MCP, a recorded edge is walked before it is saved, and save_overlay appends it"
