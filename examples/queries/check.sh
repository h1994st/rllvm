#!/usr/bin/env bash
# Asks source-level questions about a captured program.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

# rllvm-query is built only with the `query` feature, so it is absent from an
# ordinary build.
require rllvm-query python3

mkdir -p "$OUT"
rllvm-cc -g -O0 -c lib.c -o "$OUT/lib.o"
rllvm-cc -g -O0 -c app.c -o "$OUT/app.o"
rllvm-cc "$OUT/lib.o" "$OUT/app.o" -o "$OUT/app"
rllvm-info "$OUT/app" --json >"$OUT/catalog.json"

ask() {
    rllvm-query --catalog "$OUT/catalog.json" "$@"
}

# One field per question, so a wrong answer names the question that gave it.
field() {
    python3 -c '
import json, sys
answer = json.load(sys.stdin)
print(eval(sys.argv[1], {"a": answer}))
' "$1"
}

defs=$(ask defs helper | field 'len(a["results"])')
[ "$defs" = 1 ] || fail "defs helper found $defs definitions, expected 1"

caller=$(ask callers helper | field 'a["results"][0]["function"]["symbol"]')
case $caller in
*main) ;;
*) fail "callers helper named $caller, expected main" ;;
esac

callee=$(ask callees main | field 'a["results"][0]["target"]["callee"]["symbol"]')
case $callee in
*helper) ;;
*) fail "callees main named $callee, expected helper" ;;
esac

# Every answer carries what it could not see alongside what it found.
analyzed=$(ask defs helper | field 'a["analysis"]["analyzed"]')
[ "$analyzed" = 2 ] ||
    fail "the answer reports $analyzed modules analyzed, expected 2"

echo "ok: defs, callers and callees agree, over 2 analyzed modules"
