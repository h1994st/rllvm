#!/usr/bin/env bash
# Shows that wrapper options are long-only, that every other argument reaches
# the real compiler, and that diagnostics never touch stdout.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm

# Clang's response-file syntax: one argument per line, expanded before the
# wrapper classifies anything. Written at run time because it names $OUT.
cat >"$OUT/args.rsp" <<RSP
-c
demo.c
-o
$OUT/demo.o
RSP

# Wrapper options come first and are long-only. Everything after them is the
# compiler's.
rllvm-cc --rllvm-verbose=3 "@$OUT/args.rsp" \
    >"$OUT/stdout.txt" 2>"$OUT/stderr.txt"

# The contract that matters: build systems parse compiler stdout, so even at
# the noisiest verbosity nothing may land there.
[ ! -s "$OUT/stdout.txt" ] ||
    fail "--rllvm-verbose=3 wrote to stdout: $(cat "$OUT/stdout.txt")"
[ -s "$OUT/stderr.txt" ] ||
    fail "--rllvm-verbose=3 produced no diagnostics on stderr either"

rllvm-get-bc "$OUT/demo.o" -o "$OUT/demo.bc"

defines "$("$BINDIR/llvm-nm" --defined-only "$OUT/demo.bc")" \
    ' T _?twice$' "demo.bc does not define twice; the response file was not expanded"

echo "ok: response file expanded, diagnostics on stderr only"
