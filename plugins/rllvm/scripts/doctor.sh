#!/bin/sh
# Reports what rllvm needs, what is installed, and whether the LLVM versions
# fit together. Read-only: it never writes a config and never runs a build.
#
# Output: `key: value` facts, `note:` for optional pieces, `problem:` for
# anything that blocks capture or queries, then `problems: N`. Exit 1 when N>0.

problems=0
problem() {
    echo "problem: $*"
    problems=$((problems + 1))
}

# config_value <key>: the key's value, double- or single-quoted.
config_value() {
    sed -n "s/^$1 *= *[\"']\(.*\)[\"'] *\$/\1/p" "$config"
}

# "22.1.8" -> "22"
major() {
    printf '%s\n' "$1" | sed -n 's/^\([0-9][0-9]*\).*/\1/p'
}

# llvm_version_ok <tool> <version>: true when <version> starts with a digit.
# Otherwise reports the problem naming <tool> and returns false, so the
# caller can clear its variable and newer_than_reader's empty-guard skips
# comparisons that would otherwise hand `-gt` something non-numeric.
llvm_version_ok() {
    case $2 in
    [0-9]*) return 0 ;;
    esac
    problem "cannot read an LLVM version from $1 (got '$2')"
    return 1
}

# newer_than_reader <writer> <writer LLVM> <reader> <reader LLVM>
# A reader understands bitcode from its own LLVM major and older, never newer.
newer_than_reader() {
    [ -n "$2" ] && [ -n "$4" ] || return 0
    if [ "$(major "$2")" -gt "$(major "$4")" ]; then
        problem "$1 writes LLVM $2 bitcode, which $3 (LLVM $4) cannot read"
    fi
}

for tool in rllvm-cc rllvm-cxx rllvm-rustc rllvm-get-bc rllvm-info rllvm-init rllvm-compdb; do
    if path=$(command -v "$tool"); then
        echo "$tool: $path"
    else
        problem "$tool is not on PATH"
    fi
done
if command -v rllvm-cc >/dev/null; then
    echo "rllvm version: $(rllvm-cc --rllvm-version | sed 's/^rllvm-cc //')"
fi

config=${RLLVM_CONFIG:-$HOME/.rllvm/config.toml}
echo "config: $config"
capture_llvm=
if [ -s "$config" ]; then
    for key in llvm_config_filepath clang_filepath clangxx_filepath \
        llvm_ar_filepath llvm_link_filepath llvm_objcopy_filepath; do
        value=$(config_value "$key")
        if [ -z "$value" ]; then
            [ "$key" = llvm_objcopy_filepath ] ||
                problem "$key is missing from $config"
        elif [ -f "$value" ] && [ -x "$value" ]; then
            echo "$key: $value"
        else
            problem "$key: $value is not an executable file"
        fi
    done
    llvm_config=$(config_value llvm_config_filepath)
    if [ -f "$llvm_config" ] && [ -x "$llvm_config" ]; then
        capture_llvm=$("$llvm_config" --version)
        echo "capture llvm: $capture_llvm"
        llvm_version_ok "llvm-config" "$capture_llvm" || capture_llvm=
    fi
else
    problem "no config at $config (the first wrapper run would write one; see the setup skill)"
fi

query_llvm=
if command -v rllvm-query >/dev/null; then
    query_version=$(rllvm-query --version 2>/dev/null | sed 's/^rllvm-query //')
    echo "rllvm-query version: ${query_version:-unknown}"
    query_llvm=$(rllvm-query --llvm-version)
    echo "rllvm-query llvm: $query_llvm"
    llvm_version_ok "rllvm-query" "$query_llvm" || query_llvm=
else
    echo "note: rllvm-query is not installed (needed for queries and the MCP server)"
fi

rust_llvm=
if command -v rustc >/dev/null; then
    rust_llvm=$(rustc -vV | sed -n 's/^LLVM version: //p')
    echo "rustc llvm: $rust_llvm"
    llvm_version_ok "rustc" "$rust_llvm" || rust_llvm=
else
    echo "note: rustc is not installed (needed only for Rust capture)"
fi

newer_than_reader "clang" "$capture_llvm" "rllvm-query" "$query_llvm"
newer_than_reader "rustc" "$rust_llvm" "llvm-link" "$capture_llvm"
newer_than_reader "rustc" "$rust_llvm" "rllvm-query" "$query_llvm"

echo "problems: $problems"
[ "$problems" -eq 0 ]
