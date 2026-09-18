#!/usr/bin/env bash
# Records bitcode paths relative to a root, then moves the build tree and
# extracts from its new location.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require llvm:llvm-nm

sources=$PWD
build="$OUT/build"
mkdir -p "$build"

# `pwd -P`, not `$PWD`: the root is compared against the bitcode file's real
# path, so a root reaching rllvm through a symlink -- /tmp and /var both are
# on macOS -- matches nothing and the paths are silently recorded absolute.
root=$(cd "$build" && pwd -P)

# Without a root the recorded paths are absolute, and moving the tree strands
# them.
(
    cd "$build"
    export RLLVM_BITCODE_ROOT="$root"
    rllvm-cc -c "$sources/lib.c" -o lib.o
    rllvm-cc -c "$sources/app.c" -o app.o
    rllvm-cc lib.o app.o -o app
)

moved="$OUT/moved"
mv "$build" "$moved"

# Both directions matter. If extraction succeeded here, the recorded paths
# were absolute after all and --bitcode-root below would prove nothing.
if rllvm-get-bc "$moved/app" -o "$OUT/stranded.bc" >/dev/null 2>&1; then
    fail "extraction worked without --bitcode-root; the paths were not relative"
fi

rllvm-get-bc --bitcode-root "$moved" "$moved/app" -o "$OUT/app.bc"
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/app.bc")
defines "$symbols" ' [Tt] _?helper$' "app.bc does not define helper"
defines "$symbols" ' T _?main$' "app.bc does not define main"

echo "ok: extraction failed after the move and succeeded with --bitcode-root"
