//! Whole-program LLVM bitcode generation in Rust.
//!
//! `rllvm` is a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) that provides
//! compiler wrappers to transparently build whole-program LLVM bitcode files alongside
//! normal compilation, and a tool to extract the embedded bitcode.
//!
//! # Overview
//!
//! The compiler wrappers ([`compiler_wrapper`]) intercept `clang`/`clang++` invocations,
//! run the real compiler normally, then also generate LLVM bitcode and embed the bitcode
//! file path into a special section of the output object file. The extraction tool
//! (`rllvm-get-bc`) later reads those paths and links the bitcode together.
//!
//! # Configuration
//!
//! See [`config`] for TOML-based configuration via `~/.rllvm/config.toml`.

/// Command-line argument parsing for compiler flag classification.
pub mod arg_parser;

/// Incremental bitcode cache for skipping recompilation of unchanged files.
pub mod cache;

/// Diagnostic utilities for version checking, install hints, and colored output.
pub mod diagnostics;

/// TOML-based configuration and LLVM tool path resolution.
pub mod config;

/// Compiler wrapper traits and LLVM/Clang implementation.
pub mod compiler_wrapper;

/// Bitcode file analysis via `llvm-dis`.
pub mod bitcode_info;

/// Error types used throughout the crate.
pub mod error;

/// Bitcode merge strategies (full link, partial link, archive).
pub mod merge;

/// Utility functions for command execution, file manipulation, and LLVM tools.
pub mod utils;

/// Internal constants for argument patterns, section names, and LLVM version ranges.
pub(crate) mod constants;
