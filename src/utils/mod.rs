//! Utility functions
//!
//! The public surface here is chosen deliberately. Each submodule is private,
//! so an item is only reachable from outside the crate if it is re-exported
//! with `pub` below; everything else is `pub(crate)` and can change freely.

/// Command execution utility functions
mod command_utils;
pub(crate) use command_utils::{execute_command_for_status, execute_command_for_stdout_string};

/// File-related, especially object-file-related, utility functions
mod file_utils;
pub use file_utils::{
    embed_bitcode_filepath_to_object_file, extract_bitcode_filepaths_from_object_file,
    extract_bitcode_filepaths_from_parsed_object, extract_bitcode_filepaths_from_parsed_objects,
};
pub(crate) use file_utils::{is_bitcode_file, is_object_file, recorded_bitcode_filepath};

/// LLVM-related utility functions
mod llvm_utils;
pub use llvm_utils::{
    archive_bitcode_files, execute_llvm_config, find_llvm_config, link_bitcode_files,
};

/// Filepath-related utility functions
mod path_utils;
pub use path_utils::calculate_filepath_hash;
pub(crate) use path_utils::derive_object_and_bitcode_filepath;
