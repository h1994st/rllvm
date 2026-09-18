//! Whole-program LLVM bitcode generation in Rust.
//!
//! `rllvm` provides compiler wrappers that build whole-program LLVM bitcode
//! alongside a normal build, and tools to extract and analyze it. It follows
//! the `CC`/`CXX` → build → extract workflow that
//! [wllvm](https://github.com/travitch/whole-program-llvm) and
//! [gllvm](https://github.com/SRI-CSL/gllvm) established.
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

// Keeps the public surface deliberate: a `pub` item that no `pub use`
// re-exports is a mistake, not API.
#![warn(unreachable_pub)]

/// Command-line argument parsing for compiler flag classification.
/// Command-line definitions shared by the binaries and the completion
/// generator. Not a supported interface.
#[doc(hidden)]
pub mod cli;

pub mod arg_parser;

mod materialize;

pub mod compilation_database;

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

/// Versioned module catalogs shared by capture inventory and materialization.
pub mod catalog;

/// Error types used throughout the crate.
pub mod error;

/// Bitcode merge strategies (full link, partial link, archive).
pub mod merge;

/// Link-time optimization modes and bitcode-path markers.
pub mod lto;

/// Utility functions for command execution, file manipulation, and LLVM tools.
pub mod utils;

/// Internal constants for argument patterns, section names, and LLVM version ranges.
pub(crate) mod constants;

/// Source-level queries over captured bitcode, linking LLVM directly.
#[cfg(feature = "query")]
pub mod query;
