#!/usr/bin/env bash
# Captures bitcode from a Cargo build and extracts it from the linked binary.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require cargo rustc llvm:llvm-dis llvm:llvm-config

# rustc bundles its own LLVM, and an older reader cannot parse a newer
# producer's bitcode. Skip with the versions rather than fail with a parse
# error that looks like an rllvm bug. An unparseable version on either side
# is not a missing prerequisite, so it fails loudly with what was read
# instead of silently skipping blank or proceeding into that parse error.
rustc_version=$(rustc -vV)
rustc_llvm=$(sed -n 's/^LLVM version: \([0-9][0-9]*\).*/\1/p' <<<"$rustc_version")
reader_version=$("$BINDIR/llvm-config" --version)
reader_llvm=$(cut -d. -f1 <<<"$reader_version")

case $rustc_llvm in
'' | *[!0-9]*) fail "could not parse rustc's LLVM version from: $rustc_version" ;;
esac
case $reader_llvm in
'' | *[!0-9]*) fail "could not parse the reader's LLVM version from: $reader_version" ;;
esac

[ "$reader_llvm" -ge "$rustc_llvm" ] ||
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
