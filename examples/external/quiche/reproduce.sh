#!/usr/bin/env bash
# Reproduces README.md on a macOS host: the capture table at the pinned commit,
# the queries into BoringSSL, and the CVE-2026-11941 triage on 0.29.1 and
# 0.29.2, including the FFI surface scan. Work happens in $WORK, a fresh
# temporary directory by default. Takes a while: four cargo builds and one
# query per C entry point.
#
#   examples/external/quiche/reproduce.sh
set -euo pipefail

[ "$(uname -s)" = Darwin ] || {
    echo "this example links with the macOS SDK" >&2
    exit 1
}
for tool in git cargo curl rllvm-cc rllvm-rustc rllvm-get-bc rllvm-info rllvm-query; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not on PATH" >&2
        exit 1
    }
done

# LLVM's own llvm-nm, from the toolchain rllvm is configured with: a system nm
# on an older LLVM rejects the bitcode rustc embeds in std and core.
config=${RLLVM_CONFIG:-$HOME/.rllvm/config.toml}
llvm_bindir=$(dirname "$(sed -n "s/^llvm_config_filepath *= *[\"']\(.*\)[\"'] *\$/\1/p" "$config")")
LLVM_NM=$llvm_bindir/llvm-nm
[ -x "$LLVM_NM" ] || {
    echo "no llvm-nm beside the configured llvm-config ($LLVM_NM)" >&2
    exit 1
}

WORK=${WORK:-$(mktemp -d)}
echo "working in $WORK"
cd "$WORK"

functions() { rllvm-info "$1" | awk '$1 == "Functions" { print $3 }'; }
fail() {
    echo "$*" >&2
    exit 1
}

git clone -q --recursive https://github.com/cloudflare/quiche
cd quiche
git checkout -q 4d23d859
git submodule update -q --init --recursive

# What ends up captured.
RUSTC_WRAPPER=rllvm-rustc cargo build -q --release -p quiche
rllvm-get-bc target/release/libquiche.rlib -o rlib.bc
rllvm-get-bc target/release/libquiche.a -o rust.bc
# Cargo does not rebuild BoringSSL when only the compilers change.
cargo clean -q
CC=rllvm-cc CXX=rllvm-cxx RUSTC_WRAPPER=rllvm-rustc cargo build -q --release -p quiche
rllvm-get-bc target/release/libquiche.a -o both.bc
echo "capture: rlib $(functions rlib.bc), staticlib $(functions rust.bc), with CC and CXX $(functions both.bc) functions"

# Queries across the FFI boundary.
rllvm-get-bc target/release/libquiche.a --output-dir release-cat
callers=$(rllvm-query --catalog release-cat/catalog.json callers SSL_do_handshake)
grep -q 'quiche::tls::Handshake' <<<"$callers" || fail "no Rust caller of SSL_do_handshake"
grep -q '^SSL_accept' <<<"$callers" || fail "no C++ caller of SSL_do_handshake"
handshake='<quiche::tls::Handshake>::do_handshake'
if rllvm-query --catalog release-cat/catalog.json --json reach "$handshake" ssl_send_alert |
    grep -q '"results": null'; then
    fail "reach found no path from $handshake into BoringSSL"
fi
rllvm-query --catalog release-cat/catalog.json reach "$handshake" ssl_send_alert

# The advisory triage.
triage() {
    local version=$1 catalog=$2
    git checkout -q "$version"
    git submodule update -q --init --recursive
    RUSTC_WRAPPER=rllvm-rustc CC=rllvm-cc cargo build -q -p quiche --features ffi
    MACOSX_DEPLOYMENT_TARGET=$(sw_vers -productVersion) \
        rllvm-cc -g -Iquiche/include cid_logger.c target/debug/libquiche.a -o cid_logger
    rllvm-get-bc cid_logger --output-dir "$catalog"
}
curl -fsSLO https://raw.githubusercontent.com/h1994st/rllvm/main/examples/external/quiche/cid_logger.c

triage 0.29.1 cat
query() { rllvm-query --catalog cat/catalog.json "$@"; }
query defs quiche_connection_id_iter_next | grep -q 'ffi.rs:1154' || fail "defs moved"
query callers quiche_connection_id_iter_next | grep -q 'cid_logger.c:20' || fail "callers moved"
query callees quiche_connection_id_iter_next | grep -q drop_glue || fail "0.29.1 lost its drop_glue"
query callees quiche_connection_id_iter_next

"$LLVM_NM" target/debug/libquiche.a 2>/dev/null |
    awk '$2=="T" && $3 ~ /^_quiche_/ { print substr($3, 2) }' |
    sort -u >surface.txt
candidates=()
while read -r fn; do
    out=$(query callees "$fn")
    if grep -q drop_glue <<<"$out" &&
        grep -Eq '::(as_ref|as_ptr|as_slice)$' <<<"$out"; then
        candidates+=("$fn")
    fi
done <surface.txt
echo "scan: ${#candidates[@]} of $(wc -l <surface.txt | tr -d ' ') entry points: ${candidates[*]}"

triage 0.29.2 cat-fixed
if rllvm-query --catalog cat-fixed/catalog.json callees quiche_connection_id_iter_next |
    grep -q drop_glue; then
    fail "0.29.2 still drops a ConnectionId"
fi
echo "0.29.2: no drop_glue in quiche_connection_id_iter_next"
echo "README records 557, 1697 and 7376 functions, and 7 of 169 entry points."
