#!/usr/bin/env bash
# Captures bitcode from an existing compile_commands.json, without rebuilding
# the project through the wrappers.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm llvm:clang

# Generated rather than committed: a compilation database records absolute
# paths, so a checked-in one would be wrong on every other machine.
cat >"$OUT/compile_commands.json" <<JSON
[
  {
    "directory": "$OUT",
    "file": "$PWD/demo.c",
    "command": "$BINDIR/clang -c $PWD/demo.c -o $OUT/demo.o"
  }
]
JSON

# `list` reports what would be selected, without compiling anything.
rllvm-compdb list "$OUT/compile_commands.json" >"$OUT/entries.json"
defines "$(cat "$OUT/entries.json")" '"selected_entries": 1' \
    "the listing did not select the one entry"

# `generate` compiles the selected entries and writes a catalog beside them.
rllvm-compdb generate "$OUT/compile_commands.json" \
    --output-dir "$OUT/analysis" >/dev/null

# Without this, a missing catalog surfaces as a bare NotFound from the next
# command -- no path, no hint of what generate actually produced. Report both.
if [ ! -f "$OUT/analysis/catalog.json" ]; then
    if listing=$(ls -A "$OUT/analysis" 2>/dev/null); then
        fail "generate wrote no $OUT/analysis/catalog.json; that directory holds: ${listing:-<nothing>}"
    else
        fail "generate created no output directory at $OUT/analysis"
    fi
fi

rllvm-get-bc "$OUT/analysis/catalog.json" -o "$OUT/demo.bc"

defines "$("$BINDIR/llvm-nm" --defined-only "$OUT/demo.bc")" \
    ' T _?twice$' "demo.bc does not define twice"

echo "ok: catalog generated from a compilation database, bitcode defines twice"
