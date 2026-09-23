#!/bin/sh
# Captures, extracts and (when rllvm-query is installed) queries a hello-world
# in a temporary directory, removed on exit. Stops at the first failing step
# and prints its output. Never writes a config: with none, it refuses.
# If the config sets bitcode_store_path, bitcode lands there, not here.

config=${RLLVM_CONFIG:-$HOME/.rllvm/config.toml}
if [ ! -s "$config" ]; then
    echo "fail: no config at $config; a wrapper run would write one. Configure first (setup skill)."
    exit 1
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/rllvm-smoke.XXXXXX") || exit 1
trap 'rm -rf "$work"' EXIT
cd "$work" || exit 1

# step <description> <command...>
step() {
    description=$1
    shift
    if "$@" >log 2>&1; then
        echo "ok: $description"
    else
        echo "fail: $description"
        sed 's/^/  /' log
        exit 1
    fi
}

printf 'int main(void) { return 0; }\n' >hello.c
step "rllvm-cc compiles and links C" rllvm-cc hello.c -o hello
step "rllvm-get-bc extracts the program" rllvm-get-bc hello -o hello.bc
step "rllvm-info reads the module" rllvm-info hello.bc

if command -v rustc >/dev/null; then
    printf 'fn main() {}\n' >main.rs
    step "rllvm-rustc compiles and links Rust" rllvm-rustc main.rs -o main
    step "rllvm-get-bc extracts the Rust program" rllvm-get-bc main -o main.bc
else
    echo "skip: rustc is not installed"
fi

if command -v rllvm-query >/dev/null; then
    request='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"inventory","arguments":{"artifact":"'$work'/hello"}}}'
    inventory_hello() {
        printf '%s\n' "$request" | rllvm-query mcp | grep -q '"isError":false'
    }
    step "rllvm-query inventories the program over MCP" inventory_hello
else
    echo "skip: rllvm-query is not installed"
fi
