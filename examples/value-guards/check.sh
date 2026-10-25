#!/usr/bin/env bash
# Surfaces the clamp that bounds a length handed to put, and checks that the answer
# reports it as evidence without judging the value.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-cc rllvm-info rllvm-query python3 llvm:opt llvm:llvm-nm

mkdir -p "$OUT"
rllvm-cc -g -O0 clamp.c -o "$OUT/clamp"
rllvm-info "$OUT/clamp" --json >"$OUT/catalog.json"
rllvm-cc -g -O0 offset.c -o "$OUT/offset"
rllvm-info "$OUT/offset" --json >"$OUT/offset.json"
rllvm-cc -g -O0 field.c -o "$OUT/field"
rllvm-info "$OUT/field" --json >"$OUT/field.json"

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
if grep -q 'unverified' <<<"$text"; then
    fail "the clamp on the value itself was marked unverified: $text"
fi

# 2. The same answer as JSON: one clamp, defining the length, with its bound.
guards=$(query guards --at clamp.c:14 --operand 2 --from copy --json |
    field '[(g["op"], g["relation"]) for g in a["results"]["guards"]]')
defines "$guards" "\('clamp', 'defines'\)" \
    "guards reported $guards, expected a clamp that defines the value"
bound=$(query guards --at clamp.c:14 --operand 2 --from copy --json |
    field '[g for g in a["results"]["guards"] if g["op"] == "clamp"][0]["bound"]')
defines "$bound" '64' "the clamp bound was $bound, expected CAP (64)"
verified=$(query guards --at clamp.c:14 --operand 2 --from copy --json |
    field '[g["verified"] for g in a["results"]["guards"]]')
[ "$verified" = "[True]" ] || fail "the clamp was not verified: $verified"

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

# 7. reach with --annotations returns the same path, and the annotation on the
#    call to put rides along labeled agent: a hypothesis, never proof.
plain=$(query reach main put --json | field 'a["results"]')
annotated=$(query reach main put --annotations --json)
path=$(field 'a["results"]' <<<"$annotated")
[ "$path" = "$plain" ] || fail "--annotations changed the path: $path, not $plain"
flows=$(field '[(f["source"], f["verdict"]) for f in a["uncertainty"].get("annotated_flows", [])]' \
    <<<"$annotated")
[ "$flows" = "[('agent', 'infeasible')]" ] ||
    fail "annotated_flows was $flows, expected one infeasible annotation labeled agent"

# 8. Past `if (left > MAX) return;`, `left` is in [0, MAX]; make_buf gets
#    `left + 1`. The guard carries to the argument through that affine
#    relation, without wrapping: verified.
offset() {
    rllvm-query --catalog "$OUT/offset.json" guards --at offset.c:15 --operand 1 "$@"
}
text=$(offset)
defines "$text" 'compare ugt 64' "no comparison on left: $text"
defines "$text" 'anchored = guarded \+ 1' "the relation to left + 1 was not shown: $text"
if grep -q 'unverified' <<<"$text"; then
    fail "the guard on left was marked unverified: $text"
fi
carried=$(offset --json |
    field '[(g["op"], g["bound"], g["verified"], g["affine"]["scale"], g["affine"]["offset"]) for g in a["results"]["guards"]]')
[ "$carried" = "[('compare', '64', True, '1', '1')]" ] ||
    fail "guards on left + 1 reported $carried, expected one verified compare with affine {1, 1}"

# 9. `b->len` is stored the clamp and read straight back for put, with nothing
#    between that writes memory: the load is the stored value, so the clamp
#    bounds the argument itself, verified.
field_guards() {
    rllvm-query --catalog "$OUT/field.json" guards --at field.c:19 --operand 2 "$@"
}
text=$(field_guards)
defines "$text" 'clamp umin 64' "no clamp through the field: $text"
defines "$text" 'on the anchor' "the clamp through the field was not on the anchor: $text"
defines "$text" 'field\.c:18' "the clamp did not name the line that stores it: $text"
if grep -q 'unverified' <<<"$text"; then
    fail "the clamp through the field was marked unverified: $text"
fi
through=$(field_guards --json |
    field '([(g["op"], g["bound"], g["verified"], g["depth"]) for g in a["results"]["guards"]], a["uncertainty"].get("value_trace_lost"))')
[ "$through" = "([('clamp', '64', True, 0)], None)" ] ||
    fail "guards on b->len reported $through, expected one verified clamp and no lost trace"

echo "ok: guards reports the clamp that defines the length handed to put, nothing for an unconstrained value, a recorded verdict, an annotated reach that keeps its path, a verified bound carried to left + 1, and a clamp followed through a struct field"
