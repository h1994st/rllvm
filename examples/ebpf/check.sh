#!/usr/bin/env bash
# Builds an eBPF object through rllvm and extracts its bitcode. eBPF needs no
# special handling: libbpf skips rllvm's section on load, and the linker keeps
# it.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:clang llvm:llvm-dis target:bpf

# -g because the BPF linker requires BTF.
rllvm-cc --target=bpf -O2 -g -c prog.c -o "$OUT/prog.o"
rllvm-get-bc "$OUT/prog.o" -o "$OUT/prog.bc"

disassembly=$("$BINDIR/llvm-dis" -o - "$OUT/prog.bc")
defines "$disassembly" '^target triple = "bpf"' \
    "prog.bc was not compiled for bpf"
defines "$disassembly" "^define.* @count_packet\(" \
    "prog.bc does not define count_packet"

echo "ok: $OUT/prog.bc targets bpf and defines count_packet"
