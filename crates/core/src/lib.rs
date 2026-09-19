//! Core library for rllvm: argument classification, bitcode capture, catalogs.
//!
//! `rllvm` (the wrapper binaries) and `rllvm-query` (source-level analysis) are
//! both built on this crate. A new compiler wrapper implements
//! [`compiler_wrapper::CompilerWrapper`] and reuses the argument classifier in
//! [`arg_parser`] and the recorded-path writers in [`compiler_wrapper::llvm`].

// Keeps the public surface deliberate: a `pub` item that no `pub use`
// re-exports is a mistake, not API.
#![warn(unreachable_pub)]

/// Command-line argument parsing for compiler flag classification.
pub mod arg_parser;

/// Bitcode file analysis via `llvm-dis`.
pub mod bitcode_info;

/// Incremental bitcode cache for skipping recompilation of unchanged files.
pub mod cache;

/// Versioned module catalogs shared by capture inventory and materialization.
pub mod catalog;

/// Compiler wrapper trait and the shared recorded-path writers.
pub mod compiler_wrapper;

/// TOML-based configuration and LLVM tool path resolution.
pub mod config;

/// Diagnostic utilities for version checking, install hints, and colored output.
pub mod diagnostics;

pub mod error;

pub mod lto;

/// Bitcode merge strategies (full link, partial link, archive).
pub mod merge;

/// Utility functions for command execution, file manipulation, and LLVM tools.
pub mod utils;

/// Internal constants for argument patterns, section names, and LLVM version
/// ranges. Reachable so the wrappers can share the argument tables; not a
/// supported interface.
#[doc(hidden)]
pub mod constants;

/// Bitcode-generation arguments derived from a classified command line. Not a
/// supported interface.
#[doc(hidden)]
pub mod materialize;
