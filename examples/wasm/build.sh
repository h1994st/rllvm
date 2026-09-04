#!/usr/bin/env bash
# Builds a WebAssembly module through rllvm and extracts whole-program bitcode.
set -euo pipefail

TARGET=wasm32-unknown-unknown
OUT=${1:-build}
mkdir -p "$OUT"

# One object per translation unit. Each records the path of its own bitcode in
# a custom section named .rllvm_bc.
rllvm-cc --target=$TARGET -c -o "$OUT/lib.o"  lib.c
rllvm-cc --target=$TARGET -c -o "$OUT/main.o" main.c

# wasm-ld concatenates custom sections, so the linked module lists every
# translation unit that went into it.
rllvm-cc --target=$TARGET -nostdlib -Wl,--no-entry -Wl,--export-all \
    -o "$OUT/app.wasm" "$OUT/lib.o" "$OUT/main.o"

# Whole-program bitcode for the linked module.
rllvm-get-bc "$OUT/app.wasm" -o "$OUT/app.bc"

echo "built $OUT/app.wasm and $OUT/app.bc"
