#!/usr/bin/env bash
# Builds an Objective-C target through the rllvm toolchain file and checks that
# the .m translation unit reached the extracted bitcode.
set -euo pipefail

OUT=${1:-build}
BINDIR=${LLVM_BINDIR:-$(llvm-config --bindir 2>/dev/null || true)}

[ "$(uname -s)" = Darwin ] \
    || { echo "needs macOS: this example links -framework Foundation"; exit 77; }
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

# An Objective-C method, not main: a .m built by the system compiler would
# still leave a main behind, so main alone would pass on a silent miss.
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/hello.bc")
grep -q '\-\[Greeter greet:\]' <<<"$symbols" \
    || { echo "hello.bc has no Objective-C method; the .m was not captured" >&2; exit 1; }

echo "ok: $OUT/hello.bc defines -[Greeter greet:]"
