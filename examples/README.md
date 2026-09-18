# Examples

Each subdirectory is a self-contained tutorial: sources, a `README.md`
walking through the flow, and a `check.sh` that runs it and verifies the
result. Every example directory must ship a `check.sh`.

## `check.sh` contract

- `$1` is an absolute output directory, opaque to the script. Write build
  artifacts only there; never into the example's own directory, a sibling
  example, or elsewhere in the repository.
- Runs with cwd set to the example's own directory.
- `LLVM_BINDIR` is set to the configured LLVM's `bindir`.
- Exit 0 on success, with an `ok: ...` line on stdout.
- Exit 77 to skip a missing prerequisite, printing why on stdout.
- Any other exit code is a failure.

## Running

```bash
cargo test --test examples
```

runs every example's `check.sh` under an isolated `RLLVM_CONFIG` and reports
which passed, skipped, or failed.
