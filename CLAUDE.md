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
rllvm-cc --rllvm-verbose=3 -o prog main.c a.c   # no separator needed
rllvm-cc --rllvm-verbose=3 -- -o prog main.c a.c # `--` still works, for explicitness
rllvm-get-bc -vvv prog                           # Produces prog.bc
```

`--rllvm-verbose=3` is essential: the wrapper logs every sub-command it runs (`[Compiling]`, `[BitcodeGeneration]`, `[Linking]`), which is the fastest way to see what the argument parser decided.

The wrappers stand in for a compiler, so every flag clang could own reaches clang: `-c`, `-v` and `--version` are the compiler's. The wrapper's own options are long-only and prefixed `--rllvm-` (`--rllvm-compiler`, `--rllvm-verbose`, `--rllvm-help`, `--rllvm-version`). `rllvm-get-bc` is not a compiler stand-in, so it keeps ordinary `-v`.

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

Bitcode embedding has two paths, and the order matters:

1. **`llvm-objcopy --add-section`**, when `llvm_objcopy_filepath` is configured and present. It edits the file in place, copying load commands through without needing to understand them, so nothing is lost.
2. **`file_utils.rs::copy_object_file`** otherwise — it rebuilds the object through the `object` crate's writer, which is a *synthesizer*: it emits a new file from an abstract model and has no channel for input bytes it does not model. Anything outside that model is dropped.

The fallback is genuinely lossy, not merely less tidy. `LC_BUILD_VERSION` is carried across explicitly by name (without it the linker warns `no platform load command found` on every object), but other unmodeled commands still vanish — an Objective-C object loses all 8 of its `LC_LINKER_OPTION` commands, which carry autolink directives, so the result can fail to link. **Prefer objcopy; do not "simplify" by deleting that path.**

WASM never uses objcopy: it appends a custom section to the raw binary, because the `object` writer has no WASM support at all.

### Argument parsing

`arg_parser.rs` walks the argument list once, dispatching in this order (`constants.rs` holds the tables):

1. Exact match against `arg_exact_match_map()`.
2. Special case for the N-ary `-Wl,--start-group` … `-Wl,--end-group`.
3. Linear scan of `arg_patterns()` regexes.
4. Fallback: `is_object_file()` (which parses the file) decides object file vs. unrecognized compile flag.

Each entry is `(arity, handler_fn_pointer)`. **Arity drives consumption independently of the handler**: the parser skips `arity` following arguments regardless of what the handler does with them, so a wrong arity silently swallows the next argument — several historical bugs came from exactly this. Handlers sort flags into `compile_args` (used for bitcode generation), `link_args` (used for the relink), `input_files`, `object_files`, or `forbidden_flags` (stripped from the user's command entirely, with a notice printed to stderr).

The step-4 fallback must stay total: `is_object_file()` returns `Ok(false)` for anything that does not parse as an object, rather than propagating. It is asked about *every* unrecognized argument, so raising there takes down builds that merely mention a linker script or an unfamiliar source extension.

### Configuration

`config.rs` reads TOML directly (via the `toml` crate — deliberately not `confy`, whose `load_path` requires `Default` and calls it to create a missing file, which forced a panic onto the first-run path). An existing file is parsed; otherwise the configuration is inferred from `llvm-config` and written out, creating parent directories.

`try_rllvm_config() -> Result<&'static RLLVMConfig, Error>` is the only accessor, caching a `Result<RLLVMConfig, String>` in a `OnceLock`. The failure is stored as a message because `OnceLock` hands out shared references and `Error` is not `Clone`. Library code returns errors rather than panicking or exiting — keep it that way. Under `#[cfg(test)]` it uses `try_default()`, re-deriving every tool path from `llvm-config`, so unit tests never read a config file.

## Conventions

- Rust edition 2024, MSRV 1.85
- Release builds use thin LTO (`[profile.dist]`); releases are cut by `cargo dist` on version tags
- Errors use a `thiserror`-derived enum in `error.rs`; library code returns `Result` rather than exiting
- Logging is `tracing` (not `log`)
- `constants.rs` is a private module — internal only, not part of the public API
- `utils/` glob-re-exports its submodules, so anything `pub` there is public API

### Commits

Use [Conventional Commits](https://www.conventionalcommits.org/): `<type>: <summary>`, matching the types already in the log (`feat`, `fix`, `refactor`, `chore`, `ci`, `doc`). Keep the body short or empty — most commits here have no body at all.
