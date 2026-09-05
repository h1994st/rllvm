//! Injecting a bitcode-path marker into an LTO object.
//!
//! Under `-flto` the compiler writes a bitcode module where the object file
//! belongs, so there is no section header to patch. The path goes in as
//! module-level assembly inside a second module, which `llvm-link` merges into
//! the first. The LTO pipeline carries it through codegen, and the linker
//! concatenates the resulting section exactly as it does for ordinary objects.

use std::{fs, path::Path, process::Command};

use crate::{
    compiler_wrapper::CompilerKind,
    config::try_rllvm_config,
    error::Error,
    lto::{marker_compile_args, marker_source},
};

/// Compile a marker module naming `bitcode` and merge it into `object`.
///
/// `compiler` and `kind` must be the wrapper's own compiler, not the
/// `clang`/`clang++` from the config: a C++ build's `compile_args` carries
/// `-std=c++17`, and clang's C driver rejects that flag outright, so a
/// fixed `clang` cannot compile a C++ project's marker. `compile_args` are
/// otherwise the user's own compile arguments, so the marker is built for
/// the same target as the object; that matters twice over: the preprocessor
/// picks the section directive from the target, and a matching datalayout
/// keeps `llvm-link` from warning on every single compile. Dependency-
/// generation flags are stripped first -- see [`marker_compile_args`] --
/// or the marker compile becomes the last writer of the user's dependency
/// file.
pub(crate) fn inject_marker(
    object: &Path,
    bitcode: &Path,
    compile_args: &[String],
    compiler: &Path,
    kind: CompilerKind,
) -> Result<(), Error> {
    // A dedicated temporary directory means every exit path -- including the
    // early returns below -- cleans up the marker source and object; nothing
    // is left in the user's build tree after a failed compile.
    let workspace = tempfile::tempdir()?;
    let extension = match kind {
        CompilerKind::Clang => "c",
        CompilerKind::ClangXX => "cpp",
    };
    let source = workspace.path().join(format!("rllvm_marker.{extension}"));
    let marker = workspace.path().join("rllvm_marker.bc");

    fs::write(&source, marker_source(bitcode))?;

    let status = Command::new(compiler)
        .args(marker_compile_args(compile_args))
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

    let config = try_rllvm_config()?;
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

    Ok(())
}
