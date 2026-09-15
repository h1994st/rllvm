//! Source-level queries over captured bitcode.

pub mod extract;
pub use extract::{ModuleFacts, llvm_version};

pub mod facts;
pub use facts::*;

pub mod load;
