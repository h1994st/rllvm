#!/usr/bin/env bash
# Builds a WebAssembly module through rllvm, extracts whole-program bitcode,
# and checks that both translation units survived the link.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require wasm-ld llvm:llvm-dis llvm:clang target:wasm32

TARGET=wasm32-unknown-unknown
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

# One function from each translation unit: that is what shows the section
# survived the link rather than only the last object contributing.
disassembly=$("$BINDIR/llvm-dis" -o - "$OUT/app.bc")
for symbol in helper entry; do
    defines "$disassembly" "^define.* @$symbol\(" "app.bc does not define $symbol"
done

echo "ok: $OUT/app.bc defines helper and entry, one from each translation unit"
