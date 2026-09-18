#!/usr/bin/env bash
# Builds this example through the rllvm toolchain file and checks that the
# extracted bitcode really came from the wrappers.
set -euo pipefail

OUT=${1:-build}
BINDIR=${LLVM_BINDIR:-$(llvm-config --bindir 2>/dev/null || true)}

command -v cmake >/dev/null \
    || { echo "cmake is not installed"; exit 77; }
[ -x "$BINDIR/llvm-nm" ] \
    || { echo "llvm-nm not found; set LLVM_BINDIR"; exit 77; }

# Absolute: a relative -DCMAKE_TOOLCHAIN_FILE only resolves against the
# working directory on CMake >= 3.21, and this example requires only 3.10.
TOOLCHAIN=$(cd ../../cmake && pwd)/rllvm-toolchain.cmake

cmake -S . -B "$OUT" \
    -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN" >/dev/null
cmake --build "$OUT" >/dev/null
rllvm-get-bc "$OUT/hello" -o "$OUT/hello.bc"

# A leading underscore on Mach-O, none on ELF.
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/hello.bc")
grep -qE ' T _?main$' <<<"$symbols" \
    || { echo "hello.bc does not define main" >&2; exit 1; }

echo "ok: $OUT/hello.bc defines main"
