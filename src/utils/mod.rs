//! Utility functions

/// File-related, especially object-file-related, utility functions
mod file_utils;
pub use file_utils::*;

/// LLVM-related utility functions
mod llvm_utils;
pub use llvm_utils::*;

/// Filepath-related utility functions
mod path_utils;
pub use path_utils::*;
