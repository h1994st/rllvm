//! Injecting a bitcode-path marker into an LTO object.
//!
//! Under `-flto` the compiler writes a bitcode module where the object file
//! belongs, so there is no section header to patch. The path goes in as
//! module-level assembly inside a second module, which `llvm-link` merges into
//! the first. The LTO pipeline carries it through codegen, and the linker
//! concatenates the resulting section exactly as it does for ordinary objects.

use std::{fs, path::Path, process::Command};

use crate::{config::try_rllvm_config, error::Error, lto::marker_source};

/// Compile a marker module naming `bitcode` and merge it into `object`.
///
/// `compile_args` are the user's own compile arguments, so the marker is built
/// for the same target as the object. That matters twice over: the
/// preprocessor picks the section directive from the target, and a matching
/// datalayout keeps `llvm-link` from warning on every single compile.
pub(crate) fn inject_marker(
    object: &Path,
    bitcode: &Path,
    compile_args: &[String],
) -> Result<(), Error> {
    let dir = object.parent().ok_or_else(|| {
        Error::InvalidArguments(format!("Object path has no parent directory: {object:?}"))
    })?;
    let stem = object.file_name().unwrap_or_default().to_string_lossy();
    let source = dir.join(format!(".{stem}.rllvm_marker.c"));
    let marker = dir.join(format!(".{stem}.rllvm_marker.bc"));

    fs::write(&source, marker_source(bitcode))?;

    let config = try_rllvm_config()?;
    let status = Command::new(config.clang_filepath())
        .args(compile_args)
        .args(["-emit-llvm", "-c", "-o"])
        .arg(&marker)
        .arg(&source)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to compile the LTO marker for {object:?}: exit_status={status}. \
             On a target that is neither ELF nor Mach-O, set lto_mode = \"skip\"."
        )));
    }

    // Reading and writing the same path is safe: `llvm-link` parses both
    // inputs before it writes the output.
    let status = Command::new(config.llvm_link_filepath())
        .arg(object)
        .arg(&marker)
        .arg("-o")
        .arg(object)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to merge the LTO marker into {object:?}: exit_status={status}"
        )));
    }

    // Best effort: a leftover marker is untidy, not incorrect.
    let _ = fs::remove_file(&source);
    let _ = fs::remove_file(&marker);

    Ok(())
}
