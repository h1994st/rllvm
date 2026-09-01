//! Diagnostic utilities for version checking, install hints, and colored output.

use std::path::Path;

use owo_colors::OwoColorize;

use crate::utils::{execute_command_for_stdout_string, execute_llvm_config};

/// Extracts the major version number from a version string like "17.0.6" or "17".
fn parse_major_version(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// Checks whether the clang and LLVM tool versions are compatible.
///
/// Queries `clang --version` and `llvm-config --version`, compares major versions,
/// and emits a colored warning if they differ.
pub fn check_version_compatibility(clang_filepath: &Path, llvm_config_filepath: &Path) {
    let clang_version = match execute_command_for_stdout_string(clang_filepath, &["--version"]) {
        Ok(output) => output,
        Err(_) => return,
    };

    let llvm_version = match execute_llvm_config(llvm_config_filepath, &["--version"]) {
        Ok(v) => v,
        Err(_) => return,
    };

    // clang --version output looks like: "clang version 17.0.6 ..."
    // Extract the version number after "version"
    let clang_ver_str = clang_version
        .lines()
        .next()
        .and_then(|line| {
            line.split_whitespace()
                .skip_while(|&w| w != "version")
                .nth(1)
        })
        .unwrap_or("");

    let clang_major = match parse_major_version(clang_ver_str) {
        Some(v) => v,
        None => return,
    };

    let llvm_major = match parse_major_version(&llvm_version) {
        Some(v) => v,
        None => return,
    };

    if clang_major != llvm_major {
        eprintln!(
            "{} clang version ({}, major={}) does not match LLVM tools version ({}, major={}). \
             This may cause compatibility issues.",
            "warning:".yellow().bold(),
            clang_ver_str,
            clang_major,
            llvm_version.trim(),
            llvm_major,
        );
    }
}

/// Returns a platform-specific install suggestion for the given tool.
pub fn install_suggestion(tool_name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("brew install llvm  # provides {tool_name}")
    } else if cfg!(target_os = "windows") {
        format!("choco install llvm  # provides {tool_name}")
    } else {
        // Linux (Debian/Ubuntu-style as most common)
        format!("sudo apt install llvm clang  # provides {tool_name}")
    }
}

/// Prints a colored error message for a missing tool with an install suggestion.
pub fn print_missing_tool_error(tool_name: &str, searched_path: Option<&Path>) {
    if let Some(path) = searched_path {
        eprintln!(
            "{} required tool `{}` not found at configured path: {}",
            "error:".red().bold(),
            tool_name.bold(),
            path.display(),
        );
    } else {
        eprintln!(
            "{} required tool `{}` not found on this system",
            "error:".red().bold(),
            tool_name.bold(),
        );
    }
    eprintln!(
        "  {} install it with: {}",
        "hint:".cyan().bold(),
        install_suggestion(tool_name),
    );
}

/// Prints a colored warning message.
pub fn print_warning(message: &str) {
    eprintln!("{} {message}", "warning:".yellow().bold());
}

/// Prints a colored error message.
pub fn print_error(message: &str) {
    eprintln!("{} {message}", "error:".red().bold());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_major_version() {
        assert_eq!(parse_major_version("17.0.6"), Some(17));
        assert_eq!(parse_major_version("18.1.0"), Some(18));
        assert_eq!(parse_major_version("15"), Some(15));
        assert_eq!(parse_major_version(""), None);
        assert_eq!(parse_major_version("abc"), None);
    }

    #[test]
    fn test_install_suggestion_contains_tool_name() {
        let suggestion = install_suggestion("llvm-config");
        assert!(suggestion.contains("llvm-config"));
        assert!(suggestion.contains("llvm"));
    }
}
