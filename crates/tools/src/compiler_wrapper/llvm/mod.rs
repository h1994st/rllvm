//! LLVM compiler wrappers (clang/clang++/rustc)

mod clang_wrapper;
pub use clang_wrapper::*;

pub(crate) mod lto_marker;

pub(crate) mod marker;

mod rustc_args;
mod rustc_marker;

mod rustc_wrapper;
pub use rustc_wrapper::*;
