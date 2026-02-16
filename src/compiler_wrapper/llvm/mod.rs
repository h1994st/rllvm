//! LLVM compiler wrappers (clang/clang++/rustc)

mod clang_wrapper;
pub use clang_wrapper::*;

mod rustc_wrapper;
pub use rustc_wrapper::*;
