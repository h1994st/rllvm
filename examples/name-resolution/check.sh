#!/usr/bin/env bash
# Names one C++ symbol three ways and checks each is resolved as its own kind.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

mkdir -p "$OUT"
rllvm-cc -g -O0 -c twice.cpp -o "$OUT/twice.o"
rllvm-cc "$OUT/twice.o" -o "$OUT/twice"
rllvm-info "$OUT/twice" --json >"$OUT/catalog.json"

# Each spelling must find the definition AND be reported as its own kind.
# Checking only that it was found would not show the tiers exist at all.
resolve() {
    rllvm-query --catalog "$OUT/catalog.json" defs "$1" |
        python3 -c '
import json, sys
answer = json.load(sys.stdin)
print(answer["resolution"][0]["matched"], len(answer["results"] or []))
'
}

while read -r expected name; do
    read -r matched found <<<"$(resolve "$name")"
    [ "$matched" = "$expected" ] ||
        fail "'$name' resolved as $matched, expected $expected"
    [ "$found" = 1 ] ||
        fail "'$name' found $found definitions, expected 1"
done <<'NAMES'
mangled _Z5twiceIiET_S0_
demangled int twice<int>(int)
fuzzy twice
NAMES

echo "ok: the mangled symbol, its demangled reading and a bare identifier all resolve"
