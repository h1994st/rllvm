# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rllvm is a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm). Compiler wrappers (`rllvm-cc`, `rllvm-cxx`, `rllvm-rustc`) build whole-program LLVM bitcode alongside a normal build; `rllvm-get-bc` extracts it and `rllvm-info` inspects it. Helpers: `rllvm-init`, `rllvm-completions`.

`rules_rllvm` is a separate, Bazel-only project that does not use these binaries. Nothing here should be changed to serve it.

## Branching

A single long-lived `main` plus short-lived feature branches. Branch off `main`, open a PR into `main`, merge, delete the branch. `main` is protected and takes PRs only.

## Build & Development Commands

```bash
cargo build                                # Build all binaries
cargo test --all                           # Unit + integration tests
cargo test --test integration              # Integration tests only
cargo test test_parsing_lto                # A single test by name
cargo clippy --all-targets -- -D warnings  # CI gate
cargo fmt --all --check                    # CI gate
```

LLVM/Clang must be installed: `brew install llvm`, or `apt install llvm llvm-dev clang libclang-dev`.

### Exercising the wrappers by hand

Point `RLLVM_CONFIG` at a scratch file so you do not clobber `~/.rllvm/config.toml`.

```bash
export RLLVM_CONFIG=/tmp/scratch/rllvm.toml
rllvm-cc --rllvm-verbose=3 -o prog main.c a.c
rllvm-get-bc -vvv prog                       # Produces prog.bc
```

`--rllvm-verbose=3` logs every sub-command the wrapper runs, which is the fastest way to see what the argument parser decided.

## Architecture

Read these three things before changing wrapper behaviour; the rest is discoverable from the code.

**The bitcode-path contract** (`wrapper.rs` + `utils/file_utils.rs`). Each source compiles to an object *and* a `.bc`. The `.bc` path is written into a dedicated object-file section, **newline-terminated** — the linker *concatenates* these sections, and that concatenation is what records which translation units make up a binary. Break the separator and multi-file builds silently yield one garbage path. Only `tests/integration.rs` catches this.

**Wrappers must behave like compilers.** They stand in for `cc`, so every flag the compiler could own reaches it — `-c`, `-v`, `--help`, `--version` included. Build systems identify the compiler with `$CC --version`; answering it ourselves breaks configure scripts in ways that look nothing like an argument bug. Wrapper options are long-only and prefixed `--rllvm-`. Diagnostics go to stderr, never stdout.

**Arity drives argument consumption** (`arg_parser.rs`, tables in `constants.rs`). The parser skips `arity` arguments regardless of what the handler does, so a wrong arity silently swallows the next argument — several past bugs came from exactly this. The final fallback must stay total: `is_object_file()` returns `Ok(false)` for anything unrecognised rather than propagating, because it is asked about *every* unknown argument.

Two things that look like bugs and are not: link mode compiles each translation unit more than once (known, see #51), and bitcode embedding prefers `llvm-objcopy` over the `object`-crate rebuild because the rebuild is lossy — it drops load commands it does not model. Do not "simplify" by deleting the objcopy path.

## Conventions

- Rust edition 2024, MSRV 1.88
- Errors use a `thiserror` enum in `error.rs`; library code returns `Result` rather than exiting or panicking
- Logging is `tracing`, not `log`
- `constants.rs` is internal; `utils/` re-exports deliberately, so anything `pub` there is public API
- Releases are automated by release-please + `cargo dist`; see [RELEASING.md](RELEASING.md). Do not bump `version` by hand, and note that pushing a tag no longer releases.

### Tests

Integration tests write their own config and pass it through `RLLVM_CONFIG`, so they never read `~/.rllvm/config.toml`. Keep it that way: route every spawned binary through the `rllvm()` helper in `tests/integration.rs`.

**Confirm a new test fails before its fix.** Several tests in this repo have passed with the bug present — asserting on the wrong field, or on a value something else produced. Check that the sabotage targets the layer that can actually break.

### Writing

Keep issues, PR descriptions, and issue comments short and plain: problem, cause,
fix, verification. Cut narrative framing, rhetorical build-up, and paragraphs
justifying what the diff already shows. Tables and code blocks are fine when they
carry facts.

### Commits

[Conventional Commits](https://www.conventionalcommits.org/): `<type>: <summary>`, using the types already in the log (`feat`, `fix`, `refactor`, `chore`, `ci`, `doc`). Keep the body short or empty. PR titles follow the same format.

Releases are derived from these commits by release-please, so the type is not cosmetic — see [RELEASING.md](RELEASING.md). While below 1.0, `feat:` and `fix:` bump the patch version and **`feat!:` (or a `BREAKING CHANGE:` footer) bumps the minor**. Mark breaking changes: v0.1.7 removed `-c` and `-v` from the wrapper CLI but was committed as `feat:`, so it shipped as a patch, and that cannot be corrected after release.
