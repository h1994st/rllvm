# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rllvm is a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) that provides compiler wrappers (`rllvm-cc`, `rllvm-cxx`) to transparently build whole-program LLVM bitcode files alongside normal compilation, and a tool (`rllvm-get-bc`) to extract the embedded bitcode.

## Build & Development Commands

```bash
cargo build --verbose          # Build all binaries
cargo test --all --verbose     # Run all tests
cargo fmt --all --check        # Check formatting (CI enforces this)
cargo fmt --all                # Auto-format
```

LLVM/Clang must be installed for tests to pass. On macOS: `brew install llvm`. On Linux: `sudo apt install llvm llvm-dev clang libclang-dev`.

## Architecture

**Three binaries** (in `src/bin/`): `rllvm-cc` (clang wrapper), `rllvm-cxx` (clang++ wrapper), `rllvm-get-bc` (bitcode extractor). All three are thin CLI entry points that delegate to the library.

**Core flow**: The compiler wrappers intercept clang/clang++ invocations, run the real compiler normally, then also generate LLVM bitcode and embed the bitcode file path into a special section of the output object file. `rllvm-get-bc` later reads those paths from object files and links the bitcode together.

**Key modules**:
- `compiler_wrapper/` — `CompilerWrapper` trait and `ClangWrapper` implementation (builder pattern via `ClangWrapperBuilder`)
- `arg_parser.rs` — Parses compiler arguments into `CompilerArgsInfo`, detecting compilation mode (compiling, linking, LTO, bitcode generation) and categorizing flags
- `config.rs` — TOML-based configuration (`~/.rllvm/config.toml`) using `confy`, with `OnceLock` singleton pattern
- `constants.rs` — Argument pattern maps (HashMap + regex-based), platform-specific section names, LLVM version ranges
- `utils/` — Separated by concern: `command_utils` (process execution), `file_utils` (object file section manipulation via `object` crate), `llvm_utils` (LLVM tool discovery/invocation), `path_utils` (path resolution)

**Platform differences**: Bitcode paths are embedded in `__RLLVM,__llvm_bc` section on Darwin and `.llvm_bc` section on ELF. macOS uses glob-based heuristic search through Homebrew cellar for LLVM tools.

## Conventions

- Rust edition 2024
- Release builds use thin LTO (`[profile.dist]`)
- Errors use a custom enum (`error.rs`) with `From` trait conversions
- `constants.rs` is `pub(self)` — internal only, not part of the public API
