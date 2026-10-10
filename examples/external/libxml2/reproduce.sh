#!/usr/bin/env bash
# Reproduces README.md on a macOS host: builds libxml2 and its xml fuzzer
# through rllvm at the pinned tag, extracts the fuzzer, and runs the query.
# Work happens in $WORK, a fresh temporary directory by default.
#
#   examples/external/libxml2/reproduce.sh
set -euo pipefail

for tool in git autoconf automake glibtoolize make rllvm-cc rllvm-get-bc rllvm-info rllvm-query; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not on PATH" >&2
        exit 1
    }
done

WORK=${WORK:-$(mktemp -d)}
echo "working in $WORK"
cd "$WORK"

git clone -q --branch v2.15.4 --depth 1 https://github.com/GNOME/libxml2 2>/dev/null
cd libxml2
./autogen.sh CC=rllvm-cc CXX=rllvm-cxx --disable-shared --without-python >autogen.log 2>&1
make -j"$(sysctl -n hw.ncpu)" >build.log 2>&1
make -C fuzz xml >fuzz.log 2>&1

rllvm-get-bc fuzz/xml -o xml.bc
rllvm-get-bc fuzz/xml --output-dir cat
functions=$(rllvm-info xml.bc | awk '$1 == "Functions" { print $3 }')
modules=$(find cat -name '*.bc' | wc -l | tr -d ' ')

rllvm-query --catalog cat/catalog.json resolution-candidates >candidates.txt

# expect <description> <extended regex>: fails unless candidates.txt matches.
expect() {
    grep -Eq "$2" candidates.txt || {
        echo "expected $1" >&2
        exit 1
    }
}

expect "the xmlMalloc group" '^no field, ptr \(i64\) — 539 sites'
expect "the xmlRealloc group" '^no field, ptr \(ptr, i64\) — 178 sites'
expect "the xmlFree group" '^no field, void \(ptr\) — 1993 sites'
expect "the fuzzer's realloc" '^  candidate xmlFuzzRealloc assigned in xmlFuzzMemSetup fuzz.c:152$'
expect "libxml2's default realloc" '^  candidate realloc assigned in xmlRealloc <no location>$'

sites=$(grep -c '^  site ' candidates.txt)
groups=$(grep -Ec '^(no )?field' candidates.txt)
echo "xml.bc: $modules modules, $functions functions; $sites unresolved calls in $groups groups"
echo "README records 35 modules, 2147 functions, and 3020 unresolved calls in 77 groups."
