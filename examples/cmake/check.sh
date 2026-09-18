#!/usr/bin/env bash
# Builds this example through the rllvm toolchain file and checks that the
# extracted bitcode really came from the wrappers.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require cmake llvm:llvm-nm

# Absolute: a relative -DCMAKE_TOOLCHAIN_FILE only resolves against the
# working directory on CMake >= 3.21, and this example requires only 3.10.
TOOLCHAIN=$(cd ../../cmake && pwd)/rllvm-toolchain.cmake

cmake -S . -B "$OUT" \
    -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN" >/dev/null
cmake --build "$OUT" >/dev/null
rllvm-get-bc "$OUT/hello" -o "$OUT/hello.bc"

# A leading underscore on Mach-O, none on ELF.
defines "$("$BINDIR/llvm-nm" --defined-only "$OUT/hello.bc")" \
    ' T _?main$' "hello.bc does not define main"

echo "ok: $OUT/hello.bc defines main"
