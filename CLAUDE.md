# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Compiler wrappers (`rllvm-cc`, `rllvm-cxx`, `rllvm-rustc`) that build whole-program LLVM bitcode alongside a normal build; `rllvm-get-bc` extracts it, `rllvm-info` inspects it. Helpers: `rllvm-init`, `rllvm-completions`.

`rules_rllvm` is a separate Bazel-only project that does not use these binaries. Do not change anything here to serve it.

## Commands

```bash
cargo build
cargo test --all                           # unit + integration
cargo test parsing_lto                     # a single test by name
cargo clippy --all-targets -- -D warnings  # CI gate
cargo fmt --all --check                    # CI gate
```

LLVM/Clang required: `brew install llvm` or `apt install llvm llvm-dev clang libclang-dev`.

To try the wrappers by hand, point `RLLVM_CONFIG` at a scratch file so you do not clobber `~/.rllvm/config.toml`. `--rllvm-verbose=3` logs every sub-command, which is the fastest way to see what the argument parser decided.

## Architecture

Six things to know before changing wrapper behaviour. The rest is discoverable from the code.

**The bitcode-path contract** (`wrapper.rs`, `utils/file_utils.rs`). Each source compiles to an object *and* a `.bc`, and the `.bc` path goes into a dedicated object-file section, **newline-terminated**. The linker *concatenates* those sections, and that concatenation is what records which translation units make up a binary. Break the separator and multi-file builds silently yield one garbage path; only `tests/integration.rs` catches it.

**Section names are rllvm's own** (`constants.rs`): `__RLLVM,__rllvm_bc` on Mach-O, `.rllvm_bc` elsewhere. Never rename them to LLVM's generic ones — `wasm-ld` drops `.llvmbc` and `.llvmcmd` by name while concatenating every other custom section, so those names make WASM silently lose the paths.

**Wrappers must behave like compilers.** Every flag the compiler could own reaches it, `-c`, `-v`, `--help` and `--version` included. Build systems identify the compiler with `$CC --version`, so answering it ourselves breaks configure scripts in ways that look nothing like an argument bug. Wrapper options are long-only and prefixed `--rllvm-`. Diagnostics go to stderr, never stdout.

**Arity drives argument consumption** (`arg_parser.rs`, tables in `constants.rs`). The parser skips `arity` arguments regardless of what the handler does, so a wrong arity silently swallows the next one. The final fallback must stay total: `is_object_file()` returns `Ok(false)` for anything unrecognised, because it is asked about *every* unknown argument.

**Mach-O sections must stay `no_dead_strip`** (`utils/file_utils.rs`). Nothing references the embedded section, so a dead-stripping link discards it and extraction silently finds nothing. Every writer has to set the attribute; the alternative is deleting `-dead_strip` from the user's link, which is what rllvm used to do.

**The rustc wrapper cannot rely on `-o`** (`llvm/rustc_args.rs`). Cargo names outputs by directory and crate name, and assuming `-o` is #85. Crates that link take the path in an object added to the link, crates that archive have their members patched afterwards — never the finished binary, which breaks its Darwin code signature.

Two things that look like bugs and are not: link mode compiles each translation unit more than once (#51), and embedding prefers `llvm-objcopy` over the `object`-crate rebuild because the rebuild drops load commands it does not model. Do not delete the objcopy path.

## Conventions

- Rust edition 2024, MSRV 1.88
- Errors are a `thiserror` enum in `error.rs`; library code returns `Result` rather than exiting or panicking
- Logging is `tracing`, not `log`
- `constants.rs` is internal; anything `pub` in `utils/` is public API

### Tests

Integration tests write their own config and pass it through `RLLVM_CONFIG`, so they never read `~/.rllvm/config.toml`. Route every spawned binary through the `rllvm()` helper.

Name tests after the behaviour asserted, with no `test_` prefix (`parsing_lto`, `bitcode_store_path_relative_is_ignored`).

**Confirm a new test fails before its fix.** Several tests here have passed with the bug present. Check that the sabotage targets the layer that can actually break.

### Writing

Issues, PRs and comments: problem, cause, fix, verification. Nothing else. Cut narrative framing and paragraphs justifying what the diff already shows.

These are published under the user's identity, so write in the project's voice — state what was done, never "if you want X". Offers belong in chat.

### Commits

[Conventional Commits](https://www.conventionalcommits.org/): `<type>: <summary>` using the types already in the log. Keep the body short or empty. PR titles follow the same format.

Releases derive from these commits, so the type is not cosmetic — see [RELEASING.md](RELEASING.md). Below 1.0, `feat:`/`fix:` bump the patch and **`feat!:` bumps the minor**. Mark breaking changes: v0.1.7 removed `-c` and `-v` from the CLI but shipped as `feat:`, so it went out as a patch, and that cannot be corrected after release.

Do not bump `version` by hand, and note that pushing a tag no longer releases.
