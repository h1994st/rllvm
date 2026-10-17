#!/usr/bin/env bash
# Surfaces the clamp that bounds a length handed to put, and checks that the answer
# reports it as evidence without judging the value.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-cc rllvm-info rllvm-query python3 llvm:opt llvm:llvm-nm

mkdir -p "$OUT"
rllvm-cc -g -O0 clamp.c -o "$OUT/clamp"
rllvm-info "$OUT/clamp" --json >"$OUT/catalog.json"

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

# 1. The length passed to put on line 14 is clamped by `len < CAP ? len : CAP`.
text=$(query guards --at clamp.c:14 --operand 2 --from copy)
defines "$text" 'clamp' "no clamp guard on the length handed to put: $text"
defines "$text" 'defines' "the clamp was not reported as defining the value: $text"
defines "$text" 'clamp\.c:13' "the clamp did not name its source line: $text"
defines "$text" 'on the anchor' "the clamp was not reported on the value itself: $text"

# 2. The same answer as JSON: one clamp, defining the length, with its bound.
guards=$(query guards --at clamp.c:14 --operand 2 --from copy --json |
    field '[(g["op"], g["relation"]) for g in a["results"]["guards"]]')
defines "$guards" "\('clamp', 'defines'\)" \
    "guards reported $guards, expected a clamp that defines the value"
bound=$(query guards --at clamp.c:14 --operand 2 --from copy --json |
    field '[g for g in a["results"]["guards"] if g["op"] == "clamp"][0]["bound"]')
defines "$bound" '64' "the clamp bound was $bound, expected CAP (64)"

# 3. The same value named as a source local needs no call site.
named=$(query guards --function copy --local n --json |
    field '[g["op"] for g in a["results"]["guards"]]')
defines "$named" "'clamp'" "guards --local n reported $named, expected a clamp"

# 4. A value nothing constrains comes back empty, never as unbounded.
none=$(query guards --at clamp.c:18 --operand 2 --json |
    field 'a["results"]["guards"]')
[ "$none" = "[]" ] || fail "the unconstrained argument reported guards: $none"

# 5. The slice behind the answer is a module a tool can read.
rm -f "$OUT/guards.bc"
query guards --at clamp.c:14 --operand 2 --from copy --emit-module "$OUT/guards.bc" >/dev/null
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/guards.bc")
defines "$symbols" " [Tt] _?copy\$" "the emitted module does not define copy: $symbols"

# 6. Record the verdict the clamp supports, then confirm it: an agent's
#    hypothesis in the overlay beside the catalog, the records the README shows.
rm -f "$OUT/catalog.overlay.jsonl"
anchor='{"kind":"argument","site":"clamp.c:14","operand":2}'
echo '{"op":"annotate","anchor":'"$anchor"',"verdict":"infeasible","confidence":"high","provenance":["clamp.c:13: n = len < CAP ? len : CAP"]}' |
    query overlay record >/dev/null
echo '{"op":"verify","annotation":{"anchor":'"$anchor"'},"tool":"review","verdict":"confirmed"}' |
    query overlay record >/dev/null
listed=$(query overlay list --json |
    field '[(x["verdict"], x["verification"]["verdict"]) for x in a["annotations"]]')
[ "$listed" = "[('infeasible', 'confirmed')]" ] ||
    fail "overlay list showed $listed, expected one confirmed infeasible annotation"

echo "ok: guards reports the clamp that defines the length handed to put, nothing for an unconstrained value, and a recorded verdict lists as confirmed"
