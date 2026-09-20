#!/usr/bin/env bash
# Cross-compiles to machines this host is not, and checks each extracted
# module carries the cross target rather than the host's.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

# LLD links ELF wherever it runs; the host linker on macOS does not.
require lld llvm:llvm-nm target:aarch64 target:riscv64
mkdir -p "$OUT"

# aarch64 Linux is cross from an x86_64 Linux host by architecture, and from an
# Apple silicon host by operating system and object format. On an aarch64 Linux
# host it is neither, and the run would prove nothing.
case "$("$BINDIR/clang" -print-target-triple)" in
*aarch64*linux* | *arm64*linux*)
    skip "this host is aarch64 Linux; nothing here would be cross"
    ;;
esac

targets=(aarch64-unknown-linux-gnu)

# The embedding fallback does not model RISC-V relocations, so that target
# needs llvm-objcopy. `rllvm-init` records it whenever the tool exists.
if grep -q '^llvm_objcopy_filepath' "${RLLVM_CONFIG:-$HOME/.rllvm/config.toml}" 2>/dev/null; then
    targets+=(riscv64-unknown-linux-gnu)
else
    echo "note: llvm_objcopy_filepath is unset, skipping the RISC-V target"
fi

for triple in "${targets[@]}"; do
    app=$OUT/app-$triple

    # One invocation that compiles and links. That is the case that used to
    # fail: a relink without --target runs on the host target, handing
    # cross-built objects to the host's linker.
    rllvm-cc --target="$triple" -fuse-ld=lld -nostdlib lib.c app.c -o "$app"
    rllvm-get-bc "$app" -o "$app.bc"

    # The triple separates a real cross build from a host fallback that merely
    # succeeded.
    defines "$(rllvm-info "$app.bc")" "Target triple: +$triple\$" \
        "$app.bc is not built for $triple"

    # Both translation units: a link that dropped the recorded section would
    # leave one or both out.
    symbols=$("$BINDIR/llvm-nm" --defined-only "$app.bc")
    defines "$symbols" ' T _?_start$' "$app.bc does not define _start"
    defines "$symbols" ' [Tt] _?twice$' "$app.bc does not define twice"

    echo "ok: $app.bc is a whole-program module for $triple"
done
