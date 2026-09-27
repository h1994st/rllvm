#!/usr/bin/env bash
# Reproduces README.md on a macOS host: builds tmux through rllvm at the pinned
# commit, extracts it, and runs the queries. Work happens in $WORK, a fresh
# temporary directory by default.
#
#   examples/external/tmux/reproduce.sh
set -euo pipefail

for tool in git autoconf automake pkg-config make rllvm-cc rllvm-get-bc rllvm-info rllvm-query; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not on PATH" >&2
        exit 1
    }
done

WORK=${WORK:-$(mktemp -d)}
echo "working in $WORK"
cd "$WORK"

git clone -q https://github.com/tmux/tmux
cd tmux
git checkout -q 94796f6b
sh autogen.sh >autogen.log 2>&1
./configure CC=rllvm-cc --disable-jemalloc >configure.log
make -j"$(sysctl -n hw.ncpu)" >build.log 2>&1

rllvm-get-bc tmux -o tmux.bc
rllvm-get-bc tmux --output-dir cat
functions=$(rllvm-info tmux.bc | awk '$1 == "Functions" { print $3 }')
query() { rllvm-query --catalog cat/catalog.json "$@"; }

# expect <description> <extended regex> <query>...: fails unless the query's
# text answer matches.
expect() {
    local what=$1 pattern=$2 out
    shift 2
    out=$(query "$@")
    grep -Eq "$pattern" <<<"$out" || {
        echo "$out" >&2
        echo "expected $what" >&2
        exit 1
    }
}

expect "a path to the parser" '^binding +cmd_parse_from_arguments' \
    reach server_client_dispatch cmd_parse_from_arguments
expect "no caller of cmd_kill_server_exec" '^note: no call to the target' \
    callers cmd_kill_server_exec
expect "an unresolved call in cmdq_next" 'cmd-queue.c:625 +indirect .*unresolved' \
    at cmd-queue.c 625
expect "the command tables holding cmd_kill_server_exec" \
    '^global_initializer +in cmd_kill_server_entry' uses cmd_kill_server_exec
expect "signature-matched candidates at cmdq_next" \
    'address-taken candidates: 93 of 668 ' indirect-targets cmd-queue.c:625 --heuristics
expect "no path to input_csi_dispatch" '^note: no path over resolved edges' \
    reach input_parse_buffer input_csi_dispatch
expect "an unresolved call in input_parse" 'input.c:1013 +indirect .*unresolved' \
    at input.c 1013
expect "the state tables holding input_csi_dispatch" \
    '^global_initializer +in input_state_csi_enter_table' uses input_csi_dispatch

modules=$(find cat -name '*.bc' | wc -l | tr -d ' ')
echo "tmux.bc: $modules modules, $functions functions"
echo "README records 159 modules and 2361 functions."
