#!/usr/bin/env bash
# Reproduces README.md on a macOS host: builds rust-url's `parse` fuzz target
# through rllvm at the pinned tag, extracts it, checks what the bitcode leaves
# out, and runs the queries. Work happens in $WORK, a fresh temporary directory
# by default. LLVM_BINDIR names the LLVM whose llvm-nm and llvm-cxxfilt to use.
#
#   examples/external/rust-url/reproduce.sh
set -euo pipefail

BINDIR=${LLVM_BINDIR:-$(brew --prefix llvm)/bin}
for tool in git cargo rustup nm rllvm-rustc rllvm-get-bc rllvm-info rllvm-query \
    "$BINDIR/llvm-nm" "$BINDIR/llvm-cxxfilt"; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not on PATH" >&2
        exit 1
    }
done
cargo +nightly fuzz --version >/dev/null 2>&1 || {
    echo "cargo-fuzz and a nightly toolchain are required" >&2
    exit 1
}

WORK=${WORK:-$(mktemp -d)}
echo "working in $WORK"
cd "$WORK"

git clone -q --branch v2.5.8 --depth 1 https://github.com/servo/rust-url 2>/dev/null
cd rust-url/url
RUSTC_WRAPPER=rllvm-rustc cargo +nightly fuzz build parse >fuzz-build.log 2>&1

fuzzer="fuzz/target/$(rustc +nightly -vV | awk '$1 == "host:" { print $2 }')/release/parse"
rllvm-get-bc "$fuzzer" -o parse.bc
rllvm-get-bc "$fuzzer" --output-dir cat
functions=$(rllvm-info parse.bc | awk '$1 == "Functions" { print $3 }')
modules=$(find cat -name '*.bc' | wc -l | tr -d ' ')

# Every Rust function the binary defines but the bitcode does not must belong
# to the prebuilt standard library: its crate is one of std's, or the impl is
# on a primitive type.
nm -U "$fuzzer" | awk '$2 ~ /^[Tt]$/ { sub(/^_/, "", $3); print $3 }' | sort -u >binary.txt
"$BINDIR/llvm-nm" --defined-only parse.bc | awk '$2 ~ /^[Tt]$/ { sub(/^_/, "", $3); print $3 }' |
    sort -u >bitcode.txt
outside=$(comm -23 binary.txt bitcode.txt | grep -E '17h[0-9a-f]{16}E$|^_R' |
    "$BINDIR/llvm-cxxfilt" | awk '
        {
            name = $0
            while (sub(/^([<&(\[*]|mut |const |dyn )/, "", name)) {}
            match(name, /^[A-Za-z0-9_]*/)
            crate = substr(name, RSTART, RLENGTH)
            if (crate != "" && crate !~ /^(core|std|alloc|gimli|addr2line|object|rustc_demangle|miniz_oxide|adler2|hashbrown|memchr|std_detect|panic_unwind|unwind|__rustc|[iu](8|16|32|64|128|size)|f32|f64|bool|char|str)$/)
                print
        }')
if [ -n "$outside" ]; then
    echo "$outside" >&2
    echo "expected every missing Rust function to come from the standard library" >&2
    exit 1
fi

# expect <description> <extended regex> <query>...: fails unless the query's
# text answer matches.
expect() {
    local what=$1 pattern=$2 out
    shift 2
    out=$(rllvm-query --catalog cat/catalog.json "$@")
    grep -Eq "$pattern" <<<"$out" || {
        echo "$out" >&2
        echo "expected $what" >&2
        exit 1
    }
}

expect "a path into idna" '^binding +idna::domain_to_ascii_from_cow ' \
    reach rust_fuzzer_test_input idna::domain_to_ascii_from_cow
expect "AddressSanitizer in the bitcode" 'direct +__asan_' \
    callees idna::domain_to_ascii_from_cow
expect "SanitizerCoverage in the bitcode" 'direct +__sanitizer_cov_' \
    callees idna::domain_to_ascii_from_cow

echo "parse.bc: $modules modules, $functions functions"
echo "README records 13 modules and 719 functions."
