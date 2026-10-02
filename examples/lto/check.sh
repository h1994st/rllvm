#!/usr/bin/env bash
# Builds the same program under both LTO capture modes and checks each yields
# whole-program bitcode, then extracts a static library of -flto objects.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm llvm:llvm-ar llvm:clang

# Probed with plain clang on purpose. A toolchain that cannot link -flto at all
# is a missing prerequisite; a toolchain that can, while rllvm-cc cannot, is a
# bug worth failing on. Probing with the wrapper would hide the second case.
printf 'int main(void){return 0;}\n' >"$OUT/probe.c"
"$BINDIR/clang" -flto "$OUT/probe.c" -o "$OUT/probe" 2>/dev/null ||
    skip "this toolchain cannot link -flto"

for mode in marker save-temps; do
    mkdir -p "$OUT/$mode"
    RLLVM_LTO_MODE=$mode rllvm-cc -flto lib.c app.c -o "$OUT/$mode/app"
    rllvm-get-bc "$OUT/$mode/app" -o "$OUT/$mode/app.bc"

    symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/$mode/app.bc")
    defines "$symbols" ' T _?main$' "$mode: app.bc does not define main"
    # marker reports helper as external (T); full LTO internalises it, so
    # save-temps reports it as a local symbol (t). Asserting the exact case
    # keeps a save-temps regression to marker-shaped output from passing.
    if [ "$mode" = marker ]; then
        defines "$symbols" ' T _?helper$' "$mode: app.bc does not define helper as external"
    else
        defines "$symbols" ' t _?helper$' "$mode: app.bc does not define helper as local"
    fi
done

[ -f "$OUT/marker/app.bc" ] && [ -f "$OUT/save-temps/app.bc" ] ||
    fail "marker and save-temps did not each produce app.bc"
cmp -s "$OUT/marker/app.bc" "$OUT/save-temps/app.bc" &&
    fail "marker and save-temps produced identical bitcode"

# A static library of -flto objects holds bitcode members; it extracts without
# being linked into a program first.
mkdir -p "$OUT/archive"
rllvm-cc -flto -c lib.c -o "$OUT/archive/lib.o"
"$BINDIR/llvm-ar" rcs "$OUT/archive/libhelper.a" "$OUT/archive/lib.o"
rllvm-get-bc "$OUT/archive/libhelper.a" -o "$OUT/archive/libhelper.bc"
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/archive/libhelper.bc")
defines "$symbols" ' T _?helper$' "archive: libhelper.bc does not define helper"

echo "ok: marker and save-temps both yield bitcode defining main and helper; the -flto archive defines helper"
