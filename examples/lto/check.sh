#!/usr/bin/env bash
# Builds the same program under both LTO capture modes and checks each yields
# whole-program bitcode.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm llvm:clang

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
    # Upper or lower T: full LTO internalises helper, so save-temps reports it
    # as a local symbol while marker reports it as external.
    defines "$symbols" ' [Tt] _?helper$' "$mode: app.bc does not define helper"
done

echo "ok: marker and save-temps both yield bitcode defining main and helper"
