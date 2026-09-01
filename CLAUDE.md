# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rllvm is a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) that provides compiler wrappers (`rllvm-cc`, `rllvm-cxx`) to transparently build whole-program LLVM bitcode files alongside normal compilation, and a tool (`rllvm-get-bc`) to extract the embedded bitcode.

## Build & Development Commands

```bash
cargo build                             # Build all binaries
cargo test --all                        # Run all tests
cargo test test_parsing_lto             # Run a single test by name
cargo test -- --nocapture               # Show println!/log output from tests
cargo clippy --all-targets -- -D warnings  # CI gate: must be warning-free
cargo fmt --all --check                 # CI gate
cargo fmt --all                         # Auto-format
```

LLVM/Clang must be installed for tests to pass. On macOS: `brew install llvm`. On Linux: `sudo apt install llvm llvm-dev clang libclang-dev`.

`Cargo.lock` is gitignored, so a stale local lockfile can pin dependencies that no longer compile on current rustc. If the build fails inside a transitive dependency, run `cargo update -p <crate>` rather than editing source.

### Manually exercising the wrappers

Always point `RLLVM_CONFIG` at a scratch file so you do not create or clobber the user's `~/.rllvm/config.toml`. The config is auto-generated from `llvm-config` on first use.

```bash
export RLLVM_CONFIG=/tmp/scratch/rllvm.toml
rllvm-cc -vvv -- -o prog main.c a.c    # `--` separates wrapper args from compiler args
rllvm-get-bc -vvv prog                 # Produces prog.bc
```

`-vvv` is essential: the wrapper logs every sub-command it runs (`[Compiling]`, `[BitcodeGeneration]`, `[Linking]`), which is the fastest way to see what the argument parser decided.

## Architecture

**Three binaries** (in `src/bin/`): `rllvm-cc` (clang wrapper), `rllvm-cxx` (clang++ wrapper), `rllvm-get-bc` (bitcode extractor). `rllvm_cxx.rs` reuses `rllvm_cc.rs` via `pub mod rllvm_cc;`, so both binaries share one `rllvm_main` entry point.

### The bitcode-path contract

This is the central invariant and it spans `wrapper.rs` + `file_utils.rs`:

1. For each source file, the wrapper compiles an object file *and* a `.bc` file.
2. The **absolute path** of the `.bc` file is written into a special section of the object file (`__RLLVM,__llvm_bc` on Mach-O, `.llvm_bc` on ELF).
3. When the linker merges objects, it **concatenates** those sections. Entries must therefore be newline-terminated so the reader can separate them.
4. `rllvm-get-bc` reads the section, splits on `\n`, sorts, dedups, and hands the paths to `llvm-link` (or `llvm-ar` with `-b`).

Anything that breaks the separator or the absolute-path assumption silently corrupts multi-file builds, and only shows up end-to-end — not in unit tests.

### `CompilerWrapper::run()` flow

`run()` (in `compiler_wrapper/wrapper.rs`) is two phases:

1. `build_target()` — replays the user's original command through the real compiler, so the build sees normal behavior and diagnostics.
2. `generate_bitcode_files_and_embed_filepaths()` — unless `is_bitcode_generation_skipped()` says otherwise. Per input file it compiles an intermediate object (link mode only), emits bitcode, and embeds the path. It then **relinks** the final output from those intermediate objects, discarding phase 1's binary. Net effect in link mode: every translation unit is compiled multiple times and the output is linked twice.

Bitcode embedding does *not* use `llvm-objcopy`. `file_utils.rs::copy_object_file` rebuilds the object from scratch with the `object` crate's writer and adds the section. The `llvm_objcopy_filepath` config key is required and documented but currently unused by any code path.

### Argument parsing

`arg_parser.rs` walks the argument list once, dispatching in this order (`constants.rs` holds the tables):

1. Exact match against `arg_exact_match_map()`.
2. Special case for the N-ary `-Wl,--start-group` … `-Wl,--end-group`.
3. Linear scan of `arg_patterns()` regexes.
4. Fallback: `is_object_file()` (which parses the file) decides object file vs. unrecognized compile flag.

Each table entry is `(arity, handler_fn_pointer)`. **Arity drives consumption**: the parser skips `arity` following arguments regardless of what the handler does with them, so a wrong arity silently swallows the next argument. Handlers sort flags into `compile_args` (used for bitcode generation), `link_args` (used for the relink), `input_files`, `object_files`, or `forbidden_flags` (stripped from the user's command entirely).

`CompileMode` and `is_compile_only` / `is_lto` / `is_emit_llvm` are derived purely from parsed flags and decide which artifacts get built.

### Configuration

`config.rs` loads TOML via `confy` from `$RLLVM_CONFIG`, else `~/.rllvm/config.toml`, cached in a `OnceLock`. Failures call `std::process::exit(1)` from library code rather than returning errors.

Under `#[cfg(test)]`, `rllvm_config()` returns `RLLVMConfig::default()`, which re-derives every tool path from `llvm-config`. Tests therefore never read the user's config file — but they also never exercise config parsing.

### Testing

All tests are unit tests inside `src/`; there are no integration tests, and nothing drives the three binaries end to end. `tests/` contains only fixtures, referenced through the `test_case!` macro (`utils/test_utils.rs`). Several tests shell out to real clang and write to `/tmp`. Note that `Cargo.toml` `exclude`s `tests/*` from the published crate.

### Known broken paths (verified 2026-09-01)

Do not assume these work when testing manually:

- Compile-only (`rllvm-cc -- -c foo.c`) fails. The object path derived in `path_utils.rs` ignores `-o`, and the final relink runs with an empty object list.
- Multi-source builds produce a corrupt bitcode section — absolute paths are embedded without a trailing newline, so concatenated entries glue together and `rllvm-get-bc` fails.
- A trailing value-taking flag (`-- solo.c -I`) panics on an out-of-range slice in `arg_parser.rs`.

## Conventions

- Rust edition 2024
- Release builds use thin LTO (`[profile.dist]`); releases are cut by `cargo dist` on version tags
- Errors use a `thiserror`-derived enum in `error.rs`
- `constants.rs` is a private module — internal only, not part of the public API
- `utils/` glob-re-exports its submodules, so anything `pub` there is public API

### Commits

Use [Conventional Commits](https://www.conventionalcommits.org/): `<type>: <summary>`, matching the types already in the log (`feat`, `fix`, `refactor`, `chore`, `ci`, `doc`). Keep the body short or empty — most commits here have no body at all.
