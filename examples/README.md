# Examples

Each subdirectory is a self-contained tutorial: sources, a `README.md`
walking through the flow, and a `check.sh` that runs it and verifies the
result. Every example directory must ship a `check.sh`.

The exception is [`external/`](external/), whose subject is a third-party
project this harness cannot build; those ship a `README.md` alone.

## `check.sh` contract

- `$1` is an absolute output directory, opaque to the script. Write build
  artifacts only there; never into the example's own directory, a sibling
  example, or elsewhere in the repository.
- Runs with cwd set to the example's own directory.
- `LLVM_BINDIR` is set to the configured LLVM's `bindir`.
- Exit 0 on success, with an `ok: ...` line on stdout.
- Exit 77 to skip a missing prerequisite, printing why on stdout.
- Any other exit code is a failure.
- A script still running after 5 minutes is killed, along with anything it
  started, and reported as a failure.

## `common.sh`

Each script sources `examples/common.sh`, which sets `OUT` and `BINDIR` and
provides:

- `require <dep>...` — skips unless every dependency is present. A bare name
  is looked up on `PATH`, `llvm:<tool>` in the configured LLVM's bindir,
  `os:<name>` against `uname -s`, and `target:<arch>` against clang.
- `defines <text> <extended regex> <message>` — fails unless the text matches.
  It takes the text rather than a pipeline: `producer | grep -q` makes the
  producer take SIGPIPE when grep exits early, which `set -o pipefail` then
  reports as a failure.
- `skip <reason>` and `fail <reason>` — exit 77 and 1.

## Running

```bash
cargo test --test examples
```

runs every example's `check.sh` under an isolated `RLLVM_CONFIG` and reports
which passed, skipped, or failed. It also writes that record to
`target/tmp/examples-summary.txt`, which CI prints: libtest hides a passing
test's output, so without it a run that skipped most examples looks like one
that verified them all.
