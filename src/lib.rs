//! Whole Program LLVM in Rust

/// Command-line argument parser for compilers
pub mod arg_parser;

/// Compiler wrapper
pub mod compiler_wrapper;

/// Error Type
pub mod error;

/// Utility functions
pub mod utils;

/// Internal constants
pub(self) mod constants;
