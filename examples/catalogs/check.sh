#!/usr/bin/env bash
# Records what a binary was built from, then extracts a chosen subset of it.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm python3

mkdir -p "$OUT"
for unit in src/parser/parse.c src/codec/decode.c src/main.c; do
    rllvm-cc -c "$unit" -o "$OUT/$(basename "${unit%.c}").o"
done
rllvm-cc "$OUT/parse.o" "$OUT/decode.o" "$OUT/main.o" -o "$OUT/app"

# A catalog records which modules were found and where each came from.
rllvm-info "$OUT/app" --json >"$OUT/catalog.json"
modules=$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["modules"]))' \
    "$OUT/catalog.json")
[ "$modules" -eq 3 ] ||
    fail "the catalog lists $modules modules, expected one per translation unit (3)"

# Selecting by source copies just that module into a portable directory.
rllvm-get-bc "$OUT/app" --source "$PWD/src/parser/parse.c" --output-dir "$OUT/selected"
[ -f "$OUT/selected/catalog.json" ] ||
    fail "selection wrote no catalog into $OUT/selected"

rllvm-get-bc "$OUT/selected/catalog.json" -o "$OUT/selected.bc"
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/selected.bc")

# The point of selection: what was asked for is present and the rest is not.
# Asserting only `parse` would pass just as well on the whole program.
defines "$symbols" ' [Tt] _?parse$' "the selected bitcode does not define parse"
for absent in decode main; do
    if grep -qE " [Tt] _?$absent\$" <<<"$symbols"; then
        fail "the selected bitcode also carries $absent; the selector was ignored"
    fi
done

echo "ok: catalog lists 3 modules, and selecting one yields only parse"
