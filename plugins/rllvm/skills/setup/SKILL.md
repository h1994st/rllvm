---
name: setup
description: Install and configure rllvm, or diagnose it when capture or a query fails. Use when the user wants to start using rllvm, when an rllvm command reports a missing tool or config, when no bitcode was captured, or when a tool rejects a captured module.
---

# Setting up rllvm

The scripts live at `${CLAUDE_SKILL_DIR}/../../scripts/doctor.sh` and
`${CLAUDE_SKILL_DIR}/../../scripts/smoke-test.sh`. Both are read-only; neither
writes a config or runs the user's build.

## 1. Diagnose

Run `${CLAUDE_SKILL_DIR}/../../scripts/doctor.sh`. It prints facts, `note:`
lines for optional pieces, `problem:` lines, and `problems: N`. Work from its
report, not from guesses.

## 2. Install what is missing

Ask before installing anything.

```bash
brew install h1994st/tap/rllvm llvm        # or: cargo install rllvm
brew install h1994st/tap/rllvm-query       # or: cargo install rllvm-query
```

On Ubuntu/Debian, LLVM for capture comes from
`sudo apt install llvm llvm-dev clang libclang-dev`.

`cargo install rllvm-query` needs `LLVM_SYS_231_PREFIX` when `llvm-config` is
not on `PATH`. On Ubuntu/Debian, it also needs `libpolly-N-dev` alongside
`llvm-N-dev` and `libclang-N-dev`, with a matching major `N`.

## 3. Choose the LLVM by what will read the bitcode

A reader understands bitcode from its own LLVM major and older, never newer.
Readers include `rllvm-query`, `llvm-link` in the configured LLVM (which merges
Rust bitcode from `rustc`), and any analyser the user hands the module to
(PhASAR, SVF, KLEE). Capture with an LLVM no newer than the oldest reader.
`doctor.sh` flags the pairs it can see; ask about analysers it cannot.

## 4. Configure

The config is `$RLLVM_CONFIG`, else `~/.rllvm/config.toml`.

- **No config:** show `rllvm-init --dry-run` (add `--llvm-prefix <dir>` to pick
  a toolchain), then run `rllvm-init` with the same flags.
- **A config exists:** do not run `rllvm-init` over it — it rewrites the file
  and drops keys the user set by hand (`bitcode_store_path`, `cache_enabled`,
  `lto_mode`, …). Show `rllvm-init --dry-run`, and write only with the user's
  approval, carrying those keys across. Or write a separate file with
  `rllvm-init -o <path>` and set `RLLVM_CONFIG=<path>` for this project.

Any wrapper run with no config writes one from detection, so configure before
the first build.

## 5. Verify

Run `${CLAUDE_SKILL_DIR}/../../scripts/smoke-test.sh`: one `ok:` line per
step, `skip:` for optional pieces, and on failure `fail:` with the step's
output. Then run `doctor.sh` again and expect `problems: 0`.

## Troubleshooting

Start from `doctor.sh` every time.

| Symptom | Check |
| --- | --- |
| `rllvm-get-bc` finds no modules | the build used the wrappers: rerun one compile with `--rllvm-verbose=3`, or `RLLVM_LOG_LEVEL=3` under Cargo |
| An archive yields nothing under LTO | an archive of `-flto` objects, full or thin, holds bitcode, not objects; rebuild with LTO off to extract a library — a linked executable still extracts |
| A tool rejects the module | the capture LLVM is newer than that tool's; see step 3 |
| Modules reported missing | the build tree moved (build with `RLLVM_BITCODE_ROOT`, extract with `--bitcode-root`) or the bitcode was deleted |
| Stale results after a rebuild | `RLLVM_CACHE` is on and a side input preprocessing cannot see changed; rebuild with it off |
