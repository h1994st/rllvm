#!/usr/bin/env bash
# Builds an Objective-C target through the rllvm toolchain file and checks that
# the .m translation unit reached the extracted bitcode.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require os:Darwin cmake llvm:llvm-nm

# Absolute: a relative -DCMAKE_TOOLCHAIN_FILE only resolves against the
# working directory on CMake >= 3.21, and this example requires only 3.10.
TOOLCHAIN=$(cd ../../cmake && pwd)/rllvm-toolchain.cmake

cmake -S . -B "$OUT" \
    -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN" >/dev/null
cmake --build "$OUT" >/dev/null
rllvm-get-bc "$OUT/hello" -o "$OUT/hello.bc"

# An Objective-C method, not main: a .m built by the system compiler would
# still leave a main behind, so main alone would pass on a silent miss.
defines "$("$BINDIR/llvm-nm" --defined-only "$OUT/hello.bc")" \
    '\-\[Greeter greet:\]' "hello.bc has no Objective-C method; the .m was not captured"

echo "ok: $OUT/hello.bc defines -[Greeter greet:]"
