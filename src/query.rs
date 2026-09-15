//! Source-level queries over captured bitcode.

pub mod extract;
pub use extract::{ModuleFacts, llvm_version};

pub mod facts;
pub use facts::*;

pub mod load;

pub mod bind;
pub use bind::{BindingCandidate, BindingStatus, SymbolBinding};

pub mod index;
pub use index::{Direction, PathStep, ReachResult, Session};

#[cfg(test)]
pub(crate) mod testing;
