#!/usr/bin/env bash
# Shows that an answer knows whether its source still matches the bitcode.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

mkdir -p "$OUT/src"
# Built from a copy, so editing the source below never touches the example.
cp lib.c "$OUT/src/lib.c"
rllvm-cc -g -O0 -c "$OUT/src/lib.c" -o "$OUT/lib.o"
rllvm-info "$OUT/lib.o" --json >"$OUT/catalog.json"

status() {
    rllvm-query --catalog "$OUT/catalog.json" --json defs helper |
        python3 -c 'import json, sys; print(json.load(sys.stdin)["results"][0]["location"]["source_status"])'
}

# Both halves. Asserting only `modified` would pass on a query that always
# said so, and asserting only `current` would never exercise the check.
before=$(status)
[ "$before" = current ] ||
    fail "the untouched source reports $before, expected current"

printf 'int helper(int x) { return x + 2; }\n' >"$OUT/src/lib.c"

after=$(status)
[ "$after" = modified ] ||
    fail "the edited source reports $after, expected modified"

echo "ok: the location read current before the edit and modified after"
