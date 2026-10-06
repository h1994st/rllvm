#!/usr/bin/env bash
# Completes a call graph with an agent-recorded overlay edge, and checks that
# the edge is walked only on request and never reported as proof.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

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
text=$(query reach main handler --include-overlay)
defines "$text" '^agent +dispatch -> handler at .*ops\.c:10 via ops@8 \[high, unverified\]$' \
    "the agent step was not labeled: $text"
defines "$text" '^not proven: this path uses 1 agent edge\(s\)$' \
    "the answer did not say it is not proven: $text"
# Not asked for, the overlay is not walked.
path=$(query reach main handler --json | field 'a["results"]')
[ "$path" = None ] || fail "reach without --include-overlay walked the overlay: $path"

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
