#!/usr/bin/env bash
# Cross-compiles to a machine this host is not, and checks the whole-program
# bitcode comes back carrying the cross target rather than the host's.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

# LLD links ELF wherever it runs; the host linker on macOS does not.
require lld llvm:llvm-nm target:aarch64
mkdir -p "$OUT"

TRIPLE=aarch64-unknown-linux-gnu

# aarch64 Linux is cross from an x86_64 Linux host by architecture, and from an
# Apple silicon host by operating system and object format. It is neither on an
# aarch64 Linux host, where this would quietly become a native build that
# proves nothing, so skip instead of pretending.
case "$("$BINDIR/clang" -print-target-triple)" in
*aarch64*linux* | *arm64*linux*)
    skip "$TRIPLE is this host's own target; nothing would be cross"
    ;;
esac

# One invocation that compiles and links. That is the case that used to fail:
# rllvm relinks the objects it compiled, and a relink without --target runs on
# the host target, handing ELF objects to the host's linker.
rllvm-cc --target="$TRIPLE" -fuse-ld=lld -nostdlib lib.c app.c -o "$OUT/app"
rllvm-get-bc "$OUT/app" -o "$OUT/app.bc"

# The triple is what separates a real cross build from a host fallback that
# merely succeeded.
defines "$(rllvm-info "$OUT/app.bc")" "Target triple: +$TRIPLE\$" \
    "app.bc is not built for $TRIPLE"

# Both translation units, as in any capture: a link that dropped the recorded
# section would leave one or both out.
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/app.bc")
defines "$symbols" ' T _?_start$' "app.bc does not define _start"
defines "$symbols" ' [Tt] _?twice$' "app.bc does not define twice"

echo "ok: $OUT/app.bc is a whole-program module for $TRIPLE"
