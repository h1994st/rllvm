# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rllvm is a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) that provides compiler wrappers (`rllvm-cc`, `rllvm-cxx`, `rllvm-rustc`) to transparently build whole-program LLVM bitcode files alongside normal compilation, plus tools to extract (`rllvm-get-bc`) and inspect (`rllvm-info`) the embedded bitcode.

## Branching

A single long-lived `main`, plus short-lived feature branches. Branch off `main`, open a PR into `main`, merge, delete the branch. `main` is protected — it takes PRs only, so a direct push is rejected. There is no `dev` branch; do not recreate one.

## Build & Development Commands

```bash
cargo build                             # Build all binaries
cargo test --all                        # Run all tests (unit + integration)
cargo test --test integration           # Integration tests only
cargo test test_parsing_lto             # Run a single test by name
cargo test -- --nocapture               # Show output from tests
cargo clippy --all-targets -- -D warnings  # CI gate: must be warning-free
cargo fmt --all --check                 # CI gate
cargo fmt --all                         # Auto-format
```

LLVM/Clang must be installed. On macOS: `brew install llvm`. On Linux: `sudo apt install llvm llvm-dev clang libclang-dev`.

The integration tests write their own config into a temp directory and pass it through `RLLVM_CONFIG`, so they do not read (or depend on) `~/.rllvm/config.toml`. Keep it that way — route every spawned binary through the `rllvm()` helper in `tests/integration.rs` rather than `Command::new` directly.

### Manually exercising the wrappers

Point `RLLVM_CONFIG` at a scratch file so you do not clobber the user's `~/.rllvm/config.toml`; the config is auto-generated from `llvm-config` on first use (or run `rllvm-init`).

```bash
export RLLVM_CONFIG=/tmp/scratch/rllvm.toml
rllvm-cc -vvv -- -o prog main.c a.c    # `--` separates wrapper args from compiler args
rllvm-get-bc -vvv prog                 # Produces prog.bc
```

`-vvv` is essential: the wrapper logs every sub-command it runs (`[Compiling]`, `[BitcodeGeneration]`, `[Linking]`), which is the fastest way to see what the argument parser decided.

## Architecture

**Seven binaries** in `src/bin/`: the three wrappers (`rllvm-cc`, `rllvm-cxx`, `rllvm-rustc`), the extractor (`rllvm-get-bc`), and three helpers (`rllvm-init` for config generation, `rllvm-info` for bitcode analysis, `rllvm-completions` for shell completions). `rllvm_cxx.rs` reuses `rllvm_cc.rs` via `pub mod rllvm_cc;`, so both clang wrappers share one entry point.

### The bitcode-path contract

The central invariant, spanning `wrapper.rs` + `utils/file_utils.rs`:

1. For each source file, the wrapper compiles an object file *and* a `.bc` file.
2. The **absolute, newline-terminated** path of the `.bc` file is written into a dedicated section of the object file — `__RLLVM,__llvm_bc` (Mach-O), `.llvm_bc` (ELF), `.llvmbc` (COFF and WASM custom section).
3. When the linker merges objects it **concatenates** those sections, which is why every entry must end in a newline.
4. `rllvm-get-bc` reads the section, splits on `\n`, sorts, dedups, and hands the paths to `llvm-link` (or `llvm-ar` with `-b`).

Break the separator and multi-file builds silently produce one garbage path. This is only observable end-to-end, so `tests/integration.rs` is what guards it.

### Object file path resolution

The wrapper must embed the bitcode path into *the object file the compiler actually wrote*, which differs by mode (`utils/path_utils.rs`, `arg_parser.rs::artifact_filepaths`):

- **Compile-only (`-c`)**: the compiler owns the object file. With `-o`, that path; without it, `{stem}.o` in the **current working directory** (not next to the source). No link step follows.
- **Linking**: the wrapper builds its own hidden `.{stem}.o` next to the source purely to carry the bitcode path, then relinks the final output from those.

The bitcode file itself is always `.{stem}.o.bc` next to the source, unless `bitcode_store_path` is configured.

### `CompilerWrapper::run()` flow

`run()` (in `compiler_wrapper/wrapper.rs`) is two phases:

1. `build_target()` — replays the user's original command through the real compiler, so the build sees normal behavior and diagnostics.
2. `generate_bitcode_files_and_embed_filepaths()` — unless `is_bitcode_generation_skipped()` says otherwise. Per input file it optionally consults the bitcode cache (`cache.rs`), emits bitcode, and embeds the path. In link mode it then relinks the output from the intermediate objects; in compile-only mode it stops.

Note this compiles each translation unit more than once in link mode (once as part of the original command, once for the intermediate object, once for bitcode). That is known redundancy, not a bug to "fix" accidentally.

Bitcode embedding does *not* use `llvm-objcopy`. `file_utils.rs::copy_object_file` rebuilds the object with the `object` crate's writer and adds the section; WASM is special-cased because that writer has no WASM support. `llvm_objcopy_filepath` remains a required config key that no code path actually executes.

### Argument parsing

`arg_parser.rs` walks the argument list once, dispatching in this order (`constants.rs` holds the tables):

1. Exact match against `arg_exact_match_map()`.
2. Special case for the N-ary `-Wl,--start-group` … `-Wl,--end-group`.
3. Linear scan of `arg_patterns()` regexes.
4. Fallback: `is_object_file()` (which parses the file) decides object file vs. unrecognized compile flag.

Each entry is `(arity, handler_fn_pointer)`. **Arity drives consumption independently of the handler**: the parser skips `arity` following arguments regardless of what the handler does with them, so a wrong arity silently swallows the next argument — several historical bugs came from exactly this. Handlers sort flags into `compile_args` (used for bitcode generation), `link_args` (used for the relink), `input_files`, `object_files`, or `forbidden_flags` (stripped from the user's command entirely, currently without warning).

### Configuration

`config.rs` loads TOML via `confy` from `$RLLVM_CONFIG`, else `~/.rllvm/config.toml`, cached in a `OnceLock`. `rllvm_config()` panics if the config cannot be loaded or inferred. Under `#[cfg(test)]` it uses `try_default()`, re-deriving every tool path from `llvm-config`, so unit tests never read a config file.

## Conventions

- Rust edition 2024, MSRV 1.85
- Release builds use thin LTO (`[profile.dist]`); releases are cut by `cargo dist` on version tags
- Errors use a `thiserror`-derived enum in `error.rs`; library code returns `Result` rather than exiting
- Logging is `tracing` (not `log`)
- `constants.rs` is a private module — internal only, not part of the public API
- `utils/` glob-re-exports its submodules, so anything `pub` there is public API

### Commits

Use [Conventional Commits](https://www.conventionalcommits.org/): `<type>: <summary>`, matching the types already in the log (`feat`, `fix`, `refactor`, `chore`, `ci`, `doc`). Keep the body short or empty — most commits here have no body at all.
