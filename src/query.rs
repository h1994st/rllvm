//! Source-level queries over captured bitcode.

pub mod extract;
pub use extract::llvm_version;

pub mod facts;
pub use facts::*;
