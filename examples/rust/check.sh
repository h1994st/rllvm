#!/usr/bin/env bash
# Captures bitcode from a Cargo build and extracts it from the linked binary.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require cargo rustc llvm:llvm-dis llvm:llvm-config

# rustc bundles its own LLVM, and an older reader cannot parse a newer
# producer's bitcode. Skip with the versions rather than fail with a parse
# error that looks like an rllvm bug.
rustc_llvm=$(rustc -vV | sed -n 's/^LLVM version: \([0-9][0-9]*\).*/\1/p')
reader_llvm=$("$BINDIR/llvm-config" --version | cut -d. -f1)
[ "${reader_llvm:-0}" -ge "${rustc_llvm:-0}" ] ||
    skip "rustc's LLVM is $rustc_llvm but the reader is $reader_llvm"

# --locked with a committed Cargo.lock is what keeps cargo from writing into
# this directory; CARGO_TARGET_DIR sends everything else to $OUT.
CARGO_TARGET_DIR="$OUT/target" RUSTC_WRAPPER=rllvm-rustc \
    cargo build --offline --locked >/dev/null

rllvm-get-bc "$OUT/target/debug/rustdemo" -o "$OUT/rustdemo.bc"

# v0 mangling embeds a per-build crate disambiguator, so match the stable part:
# the length-prefixed crate name followed by the length-prefixed function name.
defines "$("$BINDIR/llvm-dis" -o - "$OUT/rustdemo.bc")" \
    "^define.*rustdemo[0-9]+helper" "rustdemo.bc does not define the crate's helper"

echo "ok: $OUT/rustdemo.bc defines rustdemo::helper"
