#!/usr/bin/env bash
# Reproduces README.md: builds SVF through rllvm, extracts its `wpa` tool as
# one module, and has `wpa` analyse it. On the host it builds the pinned
# container from this checkout and reruns itself inside it.
#
#   examples/external/svf/reproduce.sh
set -euo pipefail

LLVM_VERSION=22
SVF_COMMIT=f3f09503
IMAGE=rllvm:llvm${LLVM_VERSION}

if [ ! -f /.dockerenv ]; then
    here=$(cd "$(dirname "$0")" && pwd)
    docker build --build-arg LLVM_VERSION="$LLVM_VERSION" -t "$IMAGE" "$here/../../.."
    exec docker run --rm -v "$here":/example:ro "$IMAGE" -c /example/reproduce.sh
fi

set -x
cd /tmp
git clone -q https://github.com/SVF-tools/SVF
cd SVF
git checkout -q "$SVF_COMMIT"

apt-get update -qq
apt-get install -y -qq libz3-dev zlib1g-dev libzstd-dev libncurses-dev >/dev/null
rllvm-init --llvm-prefix "/usr/lib/llvm-$LLVM_VERSION"

export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DLLVM_DIR="/usr/lib/llvm-$LLVM_VERSION" -DBUILD_SHARED_LIBS=OFF \
    -DSVF_WARN_AS_ERROR=OFF
cmake --build build

rllvm-get-bc build/bin/wpa -o wpa.bc
start=$SECONDS
build/bin/wpa -ander -extapi=build/lib/extapi.bc wpa.bc >wpa.out 2>&1
elapsed=$((SECONDS - start))
set +x

# First occurrence: SVF prints its statistics once per phase.
svf_stat() { awk -v key="$1" '$1 == key { print $2; exit }' wpa.out; }
functions=$(rllvm-info wpa.bc | awk '$1 == "Functions" { print $3 }')
pointers=$(svf_stat TotalPointers)
objects=$(svf_stat TotalObjects)
[ -n "$pointers" ] && [ "$pointers" -gt 0 ] || {
    tail -20 wpa.out
    echo "wpa reported no points-to results" >&2
    exit 1
}

echo "wpa.bc: $(stat -c %s wpa.bc) bytes, $functions functions"
echo "Andersen's analysis: ${elapsed}s, $pointers pointers, $objects objects"
echo "README records 3351 functions, 377,357 pointers and 13,787 objects."
