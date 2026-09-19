//! Compiler wrapper

/// General compiler wrapper trait
mod wrapper;
pub use wrapper::*;

/// Writers that record a bitcode path into an object or a marker.
pub mod llvm;
