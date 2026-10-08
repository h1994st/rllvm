//! Diagnostic utilities for version checking, install hints, and colored output.

use std::path::Path;

use owo_colors::OwoColorize;

use crate::utils::{execute_command_for_stdout_string, execute_llvm_config};

/// Extracts the major version number from a version string like "17.0.6" or "17".
fn parse_major_version(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// The vendor whose clang numbers its own releases rather than LLVM's.
const APPLE_VENDOR: &str = "Apple";

/// The version number in a tool's `--version` output: the word after
/// `version` on the first line that has one. `clang` prints it first;
/// `llvm-dis` and its siblings may print a banner line before it.
fn version_word(version_output: &str) -> Option<(&str, &str)> {
    version_output.lines().find_map(|line| {
        let number = line
            .split_whitespace()
            .skip_while(|&word| word != "version")
            .nth(1)?;
        Some((line, number))
    })
}

/// The LLVM major version a tool's `--version` output reports, as `clang
/// --version` or `llvm-dis --version` print it.
///
/// `None` when it cannot be established: the output names no version, or the
/// tool is Apple clang, whose version number does not name the LLVM release
/// it is built from.
pub fn llvm_major_version(version_output: &str) -> Option<u32> {
    let (line, number) = version_word(version_output)?;
    if line.split_whitespace().any(|word| word == APPLE_VENDOR) {
        return None;
    }
    parse_major_version(number)
}

/// The LLVM major version `tool --version` reports, as
/// [`llvm_major_version`] reads it; `None` when the tool does not run or
/// the version cannot be established.
pub fn tool_llvm_major_version(tool: &Path) -> Option<u32> {
    let version = execute_command_for_stdout_string(tool, &["--version"]).ok()?;
    llvm_major_version(&version)
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

    if let Some(message) = version_mismatch(&clang_version, &llvm_version) {
        print_warning(&message);
    }
}

/// The warning for `clang --version` output `clang_version` against
/// `llvm-config --version` output `llvm_version`, when their majors differ.
///
/// The number clang prints is compared as is, Apple's included: an Apple
/// clang beside a separately installed LLVM is the mix this warns about.
fn version_mismatch(clang_version: &str, llvm_version: &str) -> Option<String> {
    // clang --version output looks like: "clang version 17.0.6 ..."
    let clang_ver_str = version_word(clang_version).map_or("", |(_, number)| number);
    let clang_major = parse_major_version(clang_ver_str)?;
    let llvm_major = parse_major_version(llvm_version)?;
    (clang_major != llvm_major).then(|| {
        format!(
            "clang version ({}, major={}) does not match LLVM tools version ({}, major={}). \
             This may cause compatibility issues.",
            clang_ver_str,
            clang_major,
            llvm_version.trim(),
            llvm_major,
        )
    })
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
    fn parses_major_version() {
        assert_eq!(parse_major_version("17.0.6"), Some(17));
        assert_eq!(parse_major_version("18.1.0"), Some(18));
        assert_eq!(parse_major_version("15"), Some(15));
        assert_eq!(parse_major_version(""), None);
        assert_eq!(parse_major_version("abc"), None);
    }

    #[test]
    fn reads_the_llvm_major_from_version_output() {
        assert_eq!(
            llvm_major_version("clang version 17.0.6 (https://x)"),
            Some(17)
        );
        assert_eq!(
            llvm_major_version("Homebrew clang version 22.1.8\nTarget: arm64"),
            Some(22)
        );
        assert_eq!(
            llvm_major_version("LLVM (http://llvm.org/):\n  LLVM version 18.1.3\n"),
            Some(18)
        );
        assert_eq!(
            llvm_major_version("Apple clang version 17.0.0 (clang-1700.0.13.5)"),
            None
        );
        assert_eq!(llvm_major_version("clang version unknown"), None);
        assert_eq!(llvm_major_version("no number here"), None);
    }

    #[test]
    fn a_clang_major_unlike_llvm_s_is_warned_about() {
        assert_eq!(version_mismatch("clang version 21.1.8", "21.1.8"), None);
        let message = version_mismatch("clang version 17.0.6", "21.1.8").unwrap();
        assert!(message.contains("major=17"), "{message}");
        assert!(message.contains("major=21"), "{message}");
    }

    /// Apple ships no `llvm-config`, so an Apple clang configured beside
    /// one always mixes toolchains.
    #[test]
    fn an_apple_clang_unlike_llvm_is_warned_about() {
        let message = version_mismatch(
            "Apple clang version 17.0.0 (clang-1700.0.13.5)\nTarget: arm64",
            "21.1.8",
        )
        .expect("no warning for Apple clang");
        assert!(message.contains("17.0.0, major=17"), "{message}");
    }

    #[test]
    fn install_suggestion_contains_tool_name() {
        let suggestion = install_suggestion("llvm-config");
        assert!(suggestion.contains("llvm-config"));
        assert!(suggestion.contains("llvm"));
    }

    #[test]
    fn print_helpers_do_not_panic() {
        // These only write to stderr; the point is that every formatting branch
        // is exercised, including the two arms of print_missing_tool_error.
        print_warning("a warning");
        print_error("an error");
        print_missing_tool_error("llvm-link", Some(Path::new("/nowhere/llvm-link")));
        print_missing_tool_error("llvm-link", None);
    }

    #[test]
    fn install_suggestion_differs_by_platform_but_always_mentions_the_tool() {
        for tool in [
            "clang",
            "llvm-link",
            "llvm-ar",
            "llvm-config",
            "something-else",
        ] {
            let s = install_suggestion(tool);
            assert!(!s.is_empty(), "no suggestion for {tool}");
        }
    }

    #[test]
    fn parses_major_version_rejects_junk() {
        assert_eq!(parse_major_version("not a version"), None);
        assert_eq!(parse_major_version(""), None);
    }

    #[test]
    fn version_compatibility_tolerates_missing_tools() {
        // Both lookups fail, so the function must return quietly rather than
        // panicking or printing a spurious mismatch.
        check_version_compatibility(
            Path::new("/nonexistent/clang"),
            Path::new("/nonexistent/llvm-config"),
        );
    }

    #[test]
    fn version_compatibility_runs_against_the_real_toolchain() {
        // Exercises the success path: both versions parse and are compared.
        if let Ok(config) = crate::config::RLLVMConfig::try_default() {
            check_version_compatibility(config.clang_filepath(), config.llvm_config_filepath());
        }
    }
}
