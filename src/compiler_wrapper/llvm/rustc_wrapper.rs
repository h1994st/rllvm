//! Rustc compiler wrapper for LLVM bitcode extraction
//!
//! Wraps `rustc` to transparently generate LLVM bitcode alongside normal compilation.
//! Users set `RUSTC=rllvm-rustc` (or `RUSTC_WRAPPER=rllvm-rustc`) so that Cargo invokes
//! this wrapper instead of `rustc` directly.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{error::Error, utils::embed_bitcode_filepath_to_object_file};

/// Rustc wrapper that generates LLVM bitcode alongside normal compilation.
#[derive(Debug)]
pub struct RustcWrapper {
    /// Path to the real `rustc` binary
    rustc_path: PathBuf,
    /// Whether to suppress diagnostic output
    is_silent: bool,
}

impl RustcWrapper {
    /// Create a new `RustcWrapper` that delegates to the given `rustc` binary.
    pub fn new(rustc_path: PathBuf) -> Self {
        Self {
            rustc_path,
            is_silent: false,
        }
    }

    pub fn silence(&mut self, value: bool) -> &mut Self {
        self.is_silent = value;
        self
    }

    /// Run rustc with the given arguments, also generating and embedding bitcode.
    ///
    /// 1. Invoke rustc with the original arguments (pass-through).
    /// 2. If the invocation produces object files, re-invoke rustc with `--emit=llvm-bc`
    ///    to generate bitcode, then embed the bitcode path into each object file.
    pub fn run<S>(&self, args: &[S]) -> Result<Option<i32>, Error>
    where
        S: AsRef<OsStr> + AsRef<str> + std::fmt::Debug,
    {
        // Step 1: Pass-through — run rustc with the original arguments
        let status = Command::new(&self.rustc_path)
            .args(args)
            .status()
            .map_err(Error::Io)?;

        if !status.success() {
            return Ok(status.code());
        }

        // Step 2: Determine if we should generate bitcode
        let args_str: Vec<&str> = args.iter().map(|a| <S as AsRef<str>>::as_ref(a)).collect();

        if should_skip_bitcode(&args_str) {
            return Ok(Some(0));
        }

        // Step 3: Determine the output path for the object file and derive bitcode path
        let output_path = find_output_path(&args_str);
        let output_path = match output_path {
            Some(p) => PathBuf::from(p),
            None => return Ok(Some(0)),
        };

        let bitcode_path = derive_bitcode_path(&output_path);

        if !self.is_silent {
            tracing::debug!(
                "Generating bitcode: output={:?}, bitcode={:?}",
                output_path,
                bitcode_path
            );
        }

        // Step 4: Re-invoke rustc with --emit=llvm-bc to generate bitcode
        let bc_status = self.generate_bitcode(&args_str, &bitcode_path)?;
        if bc_status != Some(0) && bc_status.is_some() {
            tracing::warn!(
                "Bitcode generation failed with exit code {:?}, skipping embedding",
                bc_status
            );
            return Ok(Some(0));
        }

        // Step 5: Embed the bitcode path into the object file
        if output_path.exists() && bitcode_path.exists() {
            if let Err(err) = embed_bitcode_filepath_to_object_file::<&Path>(
                &bitcode_path,
                &output_path,
                None,
            ) {
                tracing::warn!("Failed to embed bitcode path into object file: {}", err);
            }
        }

        Ok(Some(0))
    }

    /// Re-invoke rustc with `--emit=llvm-bc` to generate bitcode at the given path.
    fn generate_bitcode(&self, args: &[&str], bitcode_path: &Path) -> Result<Option<i32>, Error> {
        let mut bc_args: Vec<String> = Vec::new();

        for &arg in args {
            // Replace --emit=... with --emit=llvm-bc
            if arg.starts_with("--emit=") || arg.starts_with("--emit ") {
                continue;
            }
            // Replace -o <path> — we'll add our own
            if arg == "-o" {
                continue;
            }
            bc_args.push(arg.to_string());
        }

        // Remove the argument after -o (the output path)
        let mut filtered_args: Vec<String> = Vec::new();
        let mut skip_next = false;
        for arg in &args.iter().map(|a| a.to_string()).collect::<Vec<_>>() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "-o" {
                skip_next = true;
                continue;
            }
            if arg.starts_with("--emit=") || arg.starts_with("--emit ") {
                continue;
            }
            filtered_args.push(arg.clone());
        }

        filtered_args.push(format!("--emit=llvm-bc"));
        filtered_args.push("-o".to_string());
        filtered_args.push(bitcode_path.to_string_lossy().into_owned());

        if !self.is_silent {
            tracing::debug!("Bitcode generation args: {:?}", filtered_args);
        }

        let status = Command::new(&self.rustc_path)
            .args(&filtered_args)
            .status()
            .map_err(Error::Io)?;

        Ok(status.code())
    }
}

/// Determine if bitcode generation should be skipped for the given rustc args.
fn should_skip_bitcode(args: &[&str]) -> bool {
    // Skip if this is just a query invocation (--print, --version, -vV, etc.)
    if args.iter().any(|a| {
        *a == "--version"
            || *a == "-vV"
            || a.starts_with("--print")
            || *a == "--print"
            || *a == "-V"
    }) {
        tracing::debug!("Skipping bitcode: query invocation");
        return true;
    }

    // Skip if there are no source files (e.g., linking only)
    let has_source = args.iter().any(|a| {
        !a.starts_with('-')
            && (a.ends_with(".rs") || !a.contains('=') && !a.contains('/') && !a.contains('\\'))
    });

    // More specifically, check for a crate root
    let has_crate_root = args.iter().any(|a| a.ends_with(".rs"));

    if !has_crate_root {
        // Also check: if there's no .rs file but there's input via stdin or other means,
        // we skip bitcode generation for simplicity
        tracing::debug!("Skipping bitcode: no .rs source file found");
        return true;
    }

    // Skip if --emit doesn't include obj or link (i.e., not producing object files)
    let emit_values: Vec<&str> = args
        .iter()
        .filter_map(|a| a.strip_prefix("--emit="))
        .collect();

    if !emit_values.is_empty() {
        let emits_obj = emit_values
            .iter()
            .any(|v| v.split(',').any(|e| e == "obj" || e == "link" || e == "metadata,link"));
        if !emits_obj {
            tracing::debug!("Skipping bitcode: --emit does not include obj or link");
            return true;
        }
    }

    // Skip if this is a proc-macro crate (produces dylib, not useful for bitcode)
    if args
        .iter()
        .any(|a| a.starts_with("--crate-type=proc-macro") || a.starts_with("--crate-type=proc_macro"))
    {
        tracing::debug!("Skipping bitcode: proc-macro crate");
        return true;
    }

    // Check for --crate-type as separate flag
    let mut prev_was_crate_type = false;
    for arg in args {
        if prev_was_crate_type && (*arg == "proc-macro" || *arg == "proc_macro") {
            tracing::debug!("Skipping bitcode: proc-macro crate");
            return true;
        }
        prev_was_crate_type = *arg == "--crate-type";
    }

    // Let has_source serve as a check despite has_crate_root
    let _ = has_source;

    false
}

/// Find the output path (`-o <path>`) from rustc arguments.
fn find_output_path<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut prev_was_o = false;
    for arg in args {
        if prev_was_o {
            return Some(arg);
        }
        prev_was_o = *arg == "-o";
    }
    None
}

/// Derive the bitcode file path from the object file path.
/// For `/path/to/foo.o`, returns `/path/to/foo.bc`.
/// For `/path/to/libfoo.rlib`, returns `/path/to/libfoo.bc`.
fn derive_bitcode_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("bc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_bitcode_version() {
        assert!(should_skip_bitcode(&["--version"]));
        assert!(should_skip_bitcode(&["-vV"]));
        assert!(should_skip_bitcode(&["-V"]));
    }

    #[test]
    fn test_should_skip_bitcode_no_source() {
        assert!(should_skip_bitcode(&["-o", "output", "--crate-type=lib"]));
    }

    #[test]
    fn test_should_skip_bitcode_proc_macro() {
        assert!(should_skip_bitcode(&[
            "src/lib.rs",
            "--crate-type=proc-macro",
            "-o",
            "output"
        ]));
    }

    #[test]
    fn test_should_not_skip_bitcode_normal() {
        assert!(!should_skip_bitcode(&[
            "src/main.rs",
            "--crate-type=bin",
            "--emit=link",
            "-o",
            "output"
        ]));
    }

    #[test]
    fn test_should_skip_bitcode_emit_metadata_only() {
        assert!(should_skip_bitcode(&[
            "src/lib.rs",
            "--emit=metadata",
            "-o",
            "output"
        ]));
    }

    #[test]
    fn test_find_output_path() {
        assert_eq!(
            find_output_path(&["src/main.rs", "-o", "/tmp/output"]),
            Some("/tmp/output")
        );
        assert_eq!(
            find_output_path(&["src/main.rs", "--crate-type=bin"]),
            None
        );
    }

    #[test]
    fn test_derive_bitcode_path() {
        assert_eq!(
            derive_bitcode_path(Path::new("/tmp/foo.o")),
            PathBuf::from("/tmp/foo.bc")
        );
        assert_eq!(
            derive_bitcode_path(Path::new("/tmp/libfoo.rlib")),
            PathBuf::from("/tmp/libfoo.bc")
        );
        assert_eq!(
            derive_bitcode_path(Path::new("/tmp/foo")),
            PathBuf::from("/tmp/foo.bc")
        );
    }
}
