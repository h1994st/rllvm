#!/usr/bin/env bash
# Builds through LLD instead of the platform's default linker, and checks the
# recorded bitcode path survives the link.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

# One flag, two linkers: clang resolves -fuse-ld=lld to ld64.lld on macOS and
# ld.lld on Linux. `lld` is the driver both ship alongside.
require lld llvm:llvm-nm

rllvm-cc -fuse-ld=lld lib.c app.c -o "$OUT/app"
rllvm-get-bc "$OUT/app" -o "$OUT/app.bc"

# Both translation units: a linker that dropped the recorded section would
# leave one or both of these out.
symbols=$("$BINDIR/llvm-nm" --defined-only "$OUT/app.bc")
defines "$symbols" ' T _?main$' "app.bc does not define main"
defines "$symbols" ' [Tt] _?helper$' "app.bc does not define helper"

echo "ok: $OUT/app.bc survived an LLD link with both translation units"
