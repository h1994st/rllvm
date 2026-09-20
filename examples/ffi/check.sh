#!/usr/bin/env bash
# Captures a program whose call graph crosses FFI in both directions, and asks
# where each crossing happens in the source.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

# rllvm-query is its own crate, not in the workspace's default members, so it
# is absent from an ordinary build.
require rllvm-query llvm:llvm-ar llvm:llvm-nm
mkdir -p "$OUT"

# `-g` on every side. Without it the calls still resolve, but every answer
# comes back with a null location, and the source mapping is the half worth
# demonstrating.
rllvm-cc -g -c c_side.c -o "$OUT/c_side.o"
rllvm-cxx -g -c cxx_side.cc -o "$OUT/cxx_side.o"
"$BINDIR/llvm-ar" rcs "$OUT/libffidemo.a" "$OUT/c_side.o" "$OUT/cxx_side.o"
rllvm-rustc -g main.rs -o "$OUT/app" -L "$OUT" -l static=ffidemo

# Capture must not change what the program does.
printed=$("$OUT/app")
[ "$printed" = "42 126" ] || fail "app printed '$printed', expected '42 126'"

rllvm-get-bc "$OUT/app" -o "$OUT/app.bc"
rllvm-info "$OUT/app" --json >"$OUT/catalog.json"

# One module holding all three languages.
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/app.bc")
defines "$symbols" ' T _?rust_add$' "app.bc does not define rust_add (Rust)"
defines "$symbols" ' T _?c_double$' "app.bc does not define c_double (C)"
defines "$symbols" ' T _?cxx_triple$' "app.bc does not define cxx_triple (C++)"

# Rust calling C, reported at the Rust line that makes the call.
rust_to_c=$(rllvm-query --catalog "$OUT/catalog.json" callers c_double)
defines "$rust_to_c" '"file": *"main\.rs"' "no Rust call site recorded for c_double"
defines "$rust_to_c" 'main::main' "the Rust caller of c_double is not demangled"

# C calling back into Rust, reported at the C line that makes the call.
c_to_rust=$(rllvm-query --catalog "$OUT/catalog.json" callers rust_add)
defines "$c_to_rust" '"file": *"c_side\.c"' "no C call site recorded for rust_add"

echo "ok: $OUT/app.bc crosses FFI both ways, with source on each side"
