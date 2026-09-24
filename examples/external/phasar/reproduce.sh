#!/usr/bin/env bash
# Reproduces README.md: builds PhASAR through rllvm twice (with and without
# LTO), extracts phasar-cli and one library, and has phasar-cli analyze both.
# On the host it builds the pinned container from this checkout and reruns
# itself inside it.
#
#   examples/external/phasar/reproduce.sh
set -euo pipefail

LLVM_VERSION=22
PHASAR_COMMIT=3a5af4011
IMAGE=rllvm:llvm${LLVM_VERSION}

if [ ! -f /.dockerenv ]; then
    here=$(cd "$(dirname "$0")" && pwd)
    docker build --build-arg LLVM_VERSION="$LLVM_VERSION" -t "$IMAGE" "$here/../../.."
    exec docker run --rm -v "$here":/example:ro "$IMAGE" -c /example/reproduce.sh
fi

set -x
cd /tmp
git clone -q https://github.com/secure-software-engineering/phasar
cd phasar
git checkout -q "$PHASAR_COMMIT"
git submodule update -q --init

./utils/InstallAptDependencies.sh --noninteractive --llvm-version="$LLVM_VERSION" >/dev/null
rllvm-init --llvm-prefix "/usr/lib/llvm-$LLVM_VERSION"

export CC=rllvm-cc CXX=rllvm-cxx
# build <directory> <cmake flag>...
build() {
    local dir=$1
    shift
    cmake -S . -B "$dir" -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DPHASAR_LLVM_VERSION="$LLVM_VERSION.1" "$@"
    cmake --build "$dir"
}
# analyze <phasar-cli> <module>: the number of result lines, failing if none.
analyze() {
    local lines
    lines=$("$1" -m "$2" -D ifds-solvertest --auto-globals=false \
        --emit-raw-results --entry-points=__ALL__ | wc -l)
    [ "$lines" -gt 0 ] || {
        echo "phasar-cli printed nothing for $2" >&2
        exit 1
    }
    echo "$lines"
}

build build
rllvm-get-bc build/tools/phasar-cli/phasar-cli -o phasar-cli.bc
cli_lines=$(analyze build/tools/phasar-cli/phasar-cli phasar-cli.bc)

build build-nolto -DPHASAR_ALLOW_LTO_IN_RELEASE_BUILD=OFF
rllvm-get-bc build-nolto/lib/Utils/libphasar_utils.a -o phasar-utils.bc
utils_lines=$(analyze build-nolto/tools/phasar-cli/phasar-cli phasar-utils.bc)
set +x

functions() { rllvm-info "$1" | awk '$1 == "Functions" { print $3 }'; }
echo "phasar-cli.bc: $(stat -c %s phasar-cli.bc) bytes, $(functions phasar-cli.bc) functions, $cli_lines result lines"
echo "phasar-utils.bc: $(functions phasar-utils.bc) functions, $utils_lines result lines"
echo "README records 8584 functions and 4,920,885 lines; 361 functions and 268,142 lines."
