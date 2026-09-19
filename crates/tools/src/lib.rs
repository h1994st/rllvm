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
//! Argument classification, bitcode capture, catalogs and configuration live in
//! [`rllvm_core`]; this crate holds the concrete clang and rustc drivers, the
//! compilation database and the binaries.
//!
//! # Configuration
//!
//! See [`rllvm_core::config`] for TOML-based configuration via
//! `~/.rllvm/config.toml`.

// Keeps the public surface deliberate: a `pub` item that no `pub use`
// re-exports is a mistake, not API.
#![warn(unreachable_pub)]

/// Command-line definitions shared by the binaries and the completion
/// generator. Not a supported interface.
#[doc(hidden)]
pub mod cli;

pub mod compilation_database;

/// Concrete clang and rustc compiler wrappers.
pub mod compiler_wrapper;

/// Source-level queries over captured bitcode, linking LLVM directly.
#[cfg(feature = "query")]
pub mod query;
