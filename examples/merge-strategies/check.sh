#!/usr/bin/env bash
# Extracts the same program four ways and checks what each strategy produces.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm llvm:llvm-ar

# Objects land in $OUT, and the wrapper writes each module's bitcode beside
# its object, so nothing is produced inside the example directory.
mkdir -p "$OUT"
for unit in src/parser/parse.c src/codec/decode.c src/main.c; do
    rllvm-cc -c "$unit" -o "$OUT/$(basename "${unit%.c}").o"
done
rllvm-cc "$OUT/parse.o" "$OUT/decode.o" "$OUT/main.o" -o "$OUT/app"

# full: one module for the whole program. partial groups by directory and then
# links the groups, which is for link sets too large for a single llvm-link --
# the result is the same module, so both are checked the same way.
for strategy in full partial; do
    rllvm-get-bc --merge-strategy "$strategy" "$OUT/app" -o "$OUT/$strategy.bc"
    symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/$strategy.bc")
    for symbol in parse decode main; do
        defines "$symbols" " [Tt] _?$symbol\$" "$strategy.bc does not define $symbol"
    done
done

# archive: one bitcode member per object rather than one merged module, so
# llvm-nm reports each member separately.
"$BINDIR/llvm-ar" rcs "$OUT/libunits.a" "$OUT/parse.o" "$OUT/decode.o" 2>/dev/null
# -o is required: the default output name is relative to the working
# directory, which here is the example's own.
rllvm-get-bc --merge-strategy archive "$OUT/libunits.a" -o "$OUT/libunits.bca"
[ -f "$OUT/libunits.bca" ] ||
    fail "archive mode wrote no $OUT/libunits.bca"
members=$("$BINDIR/llvm-nm" --defined-only "$OUT/libunits.bca")
defines "$members" '\.bc:' "libunits.bca does not report per-member symbols"

# -m records which module each translation unit contributed, one path per line.
rllvm-get-bc -m "$OUT/app" -o "$OUT/manifest.bc"
[ -f "$OUT/manifest.bc.manifest" ] ||
    fail "-m wrote no $OUT/manifest.bc.manifest"
# grep -c, not wc -l: the manifest has no trailing newline, so wc undercounts.
listed=$(grep -c . "$OUT/manifest.bc.manifest")
[ "$listed" -eq 3 ] ||
    fail "the manifest lists $listed modules, expected one per translation unit (3)"

echo "ok: full, partial, archive and the manifest all describe the same 3 units"
