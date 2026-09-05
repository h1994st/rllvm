//! Injecting a bitcode-path marker into an LTO object.
//!
//! Under `-flto` the compiler writes a bitcode module where the object file
//! belongs, so there is no section header to patch. The path goes in as
//! module-level assembly inside a second module, which `llvm-link` merges into
//! the first. The LTO pipeline carries it through codegen, and the linker
//! concatenates the resulting section exactly as it does for ordinary objects.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    compiler_wrapper::CompilerKind,
    config::try_rllvm_config,
    error::Error,
    lto::{is_save_temps_artifact, is_saved_module, marker_compile_args, marker_source},
    utils::link_bitcode_files,
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

/// Move the module the LTO link saved to `<output>.rllvm.bc`.
///
/// `cleanup` removes the other save-temps artifacts. It is false when the user
/// asked for save-temps themselves, because then the artifacts are theirs.
pub(crate) fn collect_saved_module(output: &Path, cleanup: bool) -> Result<PathBuf, Error> {
    let darwin = cfg!(target_vendor = "apple");
    // `Path::parent` returns `Some("")`, not `None`, for a bare relative
    // filename with no directory component (e.g. `-o prog`), so a plain
    // `unwrap_or` never falls back to `.` and `fs::read_dir` is asked to open
    // an empty path.
    let dir = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let output_name = output.file_name().unwrap_or_default().to_string_lossy();
    let destination = PathBuf::from(format!("{}.rllvm.bc", output.display()));

    let mut saved = vec![];
    let mut litter = vec![];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_saved_module(&output_name, &name, darwin) {
            saved.push(entry.path());
        } else if cleanup && is_save_temps_artifact(&output_name, &name) {
            litter.push(entry.path());
        }
    }
    saved.sort();

    match saved.len() {
        0 => {
            return Err(Error::MissingFile(format!(
                "The LTO link produced no merged module for {output:?}. Expected {}.",
                if darwin {
                    format!("{output_name}.lto.opt.bc")
                } else {
                    format!("{output_name}.*.precodegen.bc")
                }
            )));
        }
        // More than one means more than one LTO partition.
        1 => fs::rename(&saved[0], &destination)?,
        _ => {
            let code = link_bitcode_files(&saved, destination.clone())?;
            if code != Some(0) {
                return Err(Error::ExecutionFailure(format!(
                    "Failed to merge {} save-temps partitions into {destination:?}: exit_status={code:?}",
                    saved.len()
                )));
            }
            for module in &saved {
                let _ = fs::remove_file(module);
            }
        }
    }

    for path in litter {
        let _ = fs::remove_file(path);
    }

    tracing::info!("Collected the LTO merged module: {:?}", destination);
    Ok(destination)
}
