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

# Capture must not change what the program does. 21 doubled through C (which
# calls back into Rust) is 42; tripled through C++ is 126, so this one line
# says every hop ran in order.
printed=$("$OUT/app")
[ "$printed" = "doubled=42 tripled=126" ] ||
    fail "app printed '$printed', so a hop across the boundary did not run"

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

# The other lead direction: a C `main` against a Rust staticlib, so the entry
# point is C rather than Rust. Only three functions land in the module --
# Rust's prebuilt std is not built through the wrapper and contributes none.
rllvm-rustc -g --crate-type staticlib rust_side.rs -o "$OUT/librustside.a"
rllvm-cc -g c_main.c "$OUT/librustside.a" -o "$OUT/app_c"

printed=$("$OUT/app_c")
[ "$printed" = "scaled=41" ] ||
    fail "app_c printed '$printed', so a hop across the boundary did not run"

rllvm-get-bc "$OUT/app_c" -o "$OUT/app_c.bc"
rllvm-info "$OUT/app_c" --json >"$OUT/catalog_c.json"

# C calling Rust, then that Rust function calling back into C.
c_lead=$(rllvm-query --catalog "$OUT/catalog_c.json" callers rust_scale)
defines "$c_lead" '"file": *"c_main\.c"' "no C call site recorded for rust_scale"

rust_back=$(rllvm-query --catalog "$OUT/catalog_c.json" callers c_offset)
defines "$rust_back" '"file": *"rust_side\.rs"' "no Rust call site recorded for c_offset"

echo "ok: both lead directions cross FFI, with source on each side"
