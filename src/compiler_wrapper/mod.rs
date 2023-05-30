//! Compiler wrapper

/// General compiler wrapper trait
mod wrapper;
pub use wrapper::*;

/// LLVM compiler wrapper (clang/clang++)
pub mod llvm;
