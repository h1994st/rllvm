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
    arg_parser::without_dependency_flags,
    compiler_wrapper::CompilerKind,
    config::try_rllvm_config,
    constants::FAT_LTO_SECTION_NAME,
    error::Error,
    lto::{is_save_temps_artifact, is_saved_module, marker_source},
    utils::{execute_command_for_status, link_bitcode_files, recorded_bitcode_filepath},
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
/// generation flags are stripped first -- see [`without_dependency_flags`] --
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
    let marker = build_marker_module(
        workspace.path(),
        object,
        bitcode,
        compile_args,
        compiler,
        kind,
    )?;

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

/// Compile the marker module into `workspace`, returning its path.
///
/// Shared by both injectors so the recorded entry, the escaping and the
/// compiler selection cannot drift between them.
fn build_marker_module(
    workspace: &Path,
    object: &Path,
    bitcode: &Path,
    compile_args: &[String],
    compiler: &Path,
    kind: CompilerKind,
) -> Result<PathBuf, Error> {
    let extension = match kind {
        CompilerKind::Clang => "c",
        CompilerKind::ClangXX => "cpp",
    };
    let source = workspace.join(format!("rllvm_marker.{extension}"));
    let marker = workspace.join("rllvm_marker.bc");

    // The same entry `embed_bitcode_filepath_to_object_file` would write, so
    // `bitcode_root` is honoured here too and one binary never mixes absolute
    // entries for its LTO units with relative ones for the rest.
    fs::write(&source, marker_source(&recorded_bitcode_filepath(bitcode)?))?;

    let status = Command::new(compiler)
        .args(without_dependency_flags(compile_args))
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

    Ok(marker)
}

/// Record the path in the bitcode half of a fat LTO object.
///
/// A fat object carries the machine code and the bitcode side by side, and the
/// linker decides at link time which one it consumes: GNU ld's plugin generates
/// code from the bitcode and discards the rest of the object, taking the
/// embedded section with it, while lld defaults to `--no-fat-lto-objects` and
/// keeps the section. The compile step cannot know which will happen, so the
/// path goes into both halves and whichever survives carries it.
pub(crate) fn inject_marker_into_fat_object(
    object: &Path,
    bitcode: &Path,
    compile_args: &[String],
    compiler: &Path,
    kind: CompilerKind,
) -> Result<(), Error> {
    let workspace = tempfile::tempdir()?;
    let marker = build_marker_module(
        workspace.path(),
        object,
        bitcode,
        compile_args,
        compiler,
        kind,
    )?;

    let config = try_rllvm_config()?;
    let Some(objcopy_filepath) = config.llvm_objcopy_filepath() else {
        // The `object`-crate fallback rebuilds the file from what it models,
        // and it does not model `.llvm.lto`. Skipping leaves the machine-code
        // half recorded, which is what a non-LTO link uses.
        tracing::warn!(
            "No llvm-objcopy configured, so the bitcode half of the fat LTO object \
             {object:?} records no path. An LTO link through GNU ld will extract nothing \
             from it; set llvm_objcopy_filepath."
        );
        return Ok(());
    };

    let extracted = workspace.path().join("fat_lto.bc");
    let merged = workspace.path().join("fat_lto_merged.bc");

    let status = execute_command_for_status(
        objcopy_filepath,
        &[
            format!(
                "--dump-section={FAT_LTO_SECTION_NAME}={}",
                extracted.display()
            ),
            object.to_string_lossy().into_owned(),
            // objcopy insists on an output; the dump is the only thing wanted,
            // so it goes to the workspace and dies with it.
            workspace
                .path()
                .join("discarded.o")
                .to_string_lossy()
                .into_owned(),
        ],
    )?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to read {FAT_LTO_SECTION_NAME} from the fat LTO object {object:?}: \
             exit_status={status}"
        )));
    }

    let status = Command::new(config.llvm_link_filepath())
        .arg(&extracted)
        .arg(&marker)
        .arg("-o")
        .arg(&merged)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to merge the LTO marker into {FAT_LTO_SECTION_NAME} of {object:?}: \
             exit_status={status}"
        )));
    }

    let status = execute_command_for_status(
        objcopy_filepath,
        &[
            format!(
                "--update-section={FAT_LTO_SECTION_NAME}={}",
                merged.display()
            ),
            object.to_string_lossy().into_owned(),
        ],
    )?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to write {FAT_LTO_SECTION_NAME} back to the fat LTO object {object:?}: \
             exit_status={status}"
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
            // The link is over, so the litter is this link's and rllvm still
            // owns it -- returning early without clearing it would leave the
            // user's build tree full of save-temps modules on top of the error.
            for path in litter {
                let _ = fs::remove_file(path);
            }
            return Err(Error::MissingFile(format!(
                "The LTO link produced no merged module for {output:?}. Expected {}. \
                 The link's inputs are probably not LTO bitcode: `-flto` in LDFLAGS alone \
                 is not enough if the objects were compiled without it. {output:?} now \
                 records {destination:?}, which does not exist, so `rllvm-get-bc` will \
                 fail on it.",
                if darwin {
                    format!("{output_name}.lto.opt.bc")
                } else {
                    format!("{output_name}.*.precodegen.bc")
                }
            )));
        }
        1 => fs::rename(&saved[0], &destination)?,
        // More than one means more than one LTO partition.
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
