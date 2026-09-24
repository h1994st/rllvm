#!/usr/bin/env bash
# Reproduces README.md on the host: builds nghttp2, nghttp3 and ngtcp2 through
# rllvm at the pinned commits, extracts each library, and runs the nghttp2
# queries. Work happens in $WORK, a fresh temporary directory by default.
#
#   examples/external/nghttp2/reproduce.sh
set -euo pipefail

for tool in git cmake ninja rllvm-cc rllvm-get-bc rllvm-info rllvm-query; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not on PATH" >&2
        exit 1
    }
done

WORK=${WORK:-$(mktemp -d)}
echo "working in $WORK"
cd "$WORK"
export CC=rllvm-cc CXX=rllvm-cxx

# build <name> <repository> <commit> <clone flag or ""> <cmake flag>...
build() {
    local name=$1 repo=$2 commit=$3 clone=$4
    shift 4
    git clone -q ${clone:+"$clone"} "https://github.com/$repo" "$name"
    git -C "$name" checkout -q "$commit"
    if [ -n "$clone" ]; then
        git -C "$name" submodule update -q --init --recursive
    fi
    cmake -S "$name" -B "$name/build" -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
        -DENABLE_LIB_ONLY=ON -DBUILD_TESTING=OFF "$@" >"$name/cmake.log"
    cmake --build "$name/build" >"$name/build.log"
    rllvm-get-bc "$name/build/lib/lib$name.a" -o "$name.bc"
    echo "$name: $(rllvm-info "$name.bc" | awk '$1 == "Functions" { print $3 }') functions"
}

build nghttp2 nghttp2/nghttp2 140157a8 "" -DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF
build nghttp3 ngtcp2/nghttp3 2304973 --recursive
build ngtcp2 ngtcp2/ngtcp2 3c23148e ""

# probe <name> <entry> <decoder> <file> <line>: one row of the README's table.
# Fails if reach finds no path or the callback site is not unresolved.
probe() {
    local name=$1 entry=$2 decoder=$3 file=$4 line=$5 path sites
    rllvm-info "$name/build/lib/lib$name.a" --json >"$name/catalog.json"
    query() { rllvm-query --catalog "$name/catalog.json" "$@"; }

    if query --json reach "$entry" "$decoder" | grep -q '"results": null'; then
        echo "$name: reach found no path from $entry to $decoder" >&2
        exit 1
    fi
    path=$(query reach "$entry" "$decoder" | awk '$1 == "call" { print $2 }' | paste -sd' ' -)

    sites=$(query at "$file" "$line")
    local unresolved functions
    unresolved=$(grep -c unresolved <<<"$sites" || true)
    functions=$(grep -v '^note:' <<<"$sites" | grep -c '^[a-z]' || true)
    [ "$unresolved" -gt 0 ] || {
        echo "$name: no unresolved callback site at $file:$line" >&2
        exit 1
    }
    echo "$name: $path -> $decoder; $file:$line has $unresolved unresolved sites in $functions functions"
}

probe nghttp2 nghttp2_session_mem_recv2 nghttp2_hd_inflate_hd_nv lib/nghttp2_session.c 3237
probe nghttp3 nghttp3_conn_read_stream2 nghttp3_qpack_decoder_read_request lib/nghttp3_conn.c 1828
probe ngtcp2 ngtcp2_conn_read_pkt_versioned ngtcp2_pkt_decode_hd_long lib/ngtcp2_conn.c 142
echo "README records 469, 393 and 799 functions, and 18/10, 2/2 and 2/2 unresolved sites/functions."
