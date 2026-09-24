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

cd nghttp2
rllvm-info build/lib/libnghttp2.a --json >catalog.json
query() { rllvm-query --catalog catalog.json "$@"; }

if query --json reach nghttp2_session_mem_recv2 nghttp2_hd_inflate_hd_nv |
    grep -q '"results": null'; then
    echo "reach found no path from nghttp2_session_mem_recv2 to the decoder" >&2
    exit 1
fi
query reach nghttp2_session_mem_recv2 nghttp2_hd_inflate_hd_nv
query callers nghttp2_hd_inflate_hd_nv

sites=$(query at lib/nghttp2_session.c 3237)
unresolved=$(grep -c unresolved <<<"$sites" || true)
handlers=$(grep -c '^[a-z]' <<<"$sites" || true)
[ "$unresolved" -gt 0 ] || {
    echo "$sites"
    echo "no unresolved callback site at nghttp2_session.c:3237" >&2
    exit 1
}
echo "nghttp2_session.c:3237: $unresolved unresolved callback sites across $handlers handlers"
echo "README records 469, 393 and 799 functions, and 18 sites across 11 handlers."
