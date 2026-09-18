# Shared helpers for the example check scripts. Sourced, never run.
#
# Sourcing keeps the caller's positional parameters, so `$1` below is the
# output directory the harness passed to the script.

OUT=${1:-build}
BINDIR=${LLVM_BINDIR:-$(llvm-config --bindir 2>/dev/null || true)}

# A prerequisite is missing. The harness reports the reason and moves on.
skip() {
    echo "$*"
    exit 77
}

# An assertion did not hold. The harness reports the example as failed.
fail() {
    echo "$*" >&2
    exit 1
}

# require <dep>... -- every dependency must be present, or the example skips.
#
#   <name>          on PATH
#   llvm:<tool>     in the configured LLVM's bindir, not whatever PATH finds
#   os:<name>       `uname -s`
#   target:<arch>   a target clang was built with
require() {
    local dep targets
    for dep in "$@"; do
        case $dep in
        llvm:*)
            [ -x "$BINDIR/${dep#llvm:}" ] ||
                skip "${dep#llvm:} not found; set LLVM_BINDIR"
            ;;
        os:*)
            [ "$(uname -s)" = "${dep#os:}" ] ||
                skip "needs ${dep#os:}, this host is $(uname -s)"
            ;;
        target:*)
            targets=$("$BINDIR/clang" --print-targets 2>/dev/null || true)
            grep -q "${dep#target:}" <<<"$targets" ||
                skip "clang has no ${dep#target:} target"
            ;;
        *)
            command -v "$dep" >/dev/null ||
                skip "$dep is not installed"
            ;;
        esac
    done
}

# defines <haystack> <extended regex> <message> -- the haystack must match.
#
# Takes the text rather than a pipeline: `producer | grep -q` makes the
# producer take SIGPIPE when grep exits early, which `set -o pipefail` then
# reports as a failure of the whole pipeline.
defines() {
    grep -qE "$2" <<<"$1" || fail "$3"
}
