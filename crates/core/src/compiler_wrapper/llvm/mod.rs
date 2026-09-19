//! Writers that record a bitcode path into an object or a marker.
//!
//! Shared by every wrapper: a new wrapper implements [`CompilerWrapper`] and
//! reuses these rather than reimplementing the recorded-path contract.
//!
//! [`CompilerWrapper`]: crate::compiler_wrapper::CompilerWrapper

pub mod lto_marker;

pub mod marker;
