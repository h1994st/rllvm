//! A marker object carrying one bitcode path.
//!
//! Two callers need the same thing — an object that contributes a bitcode path
//! to a link it is added to, and nothing else. The rustc wrapper passes it as
//! `-C link-arg`; a `save-temps` LTO link passes it as an ordinary input so
//! the finished binary names the module the linker saved.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    compiler_wrapper::CompilerKind, error::Error, lto::marker_compile_args,
    utils::embed_bitcode_filepath_to_object_file,
};

/// Compile an empty translation unit and embed the bitcode path into it.
///
/// The object goes to rustc as `-C link-arg`, so the linker concatenates its
/// section along with every other object's, and `rllvm-get-bc` finds the path
/// in the finished binary.
///
/// `compiler`, `kind` and `compile_args` decide what the object is built for.
/// A bare `clang -c` builds for the host, so an `-arch x86_64` link silently
/// drops the marker -- "ignoring file ..., found architecture 'arm64'" is a
/// warning, the link still succeeds, and the binary names nothing. The
/// arguments carry the target, and the compiler has to be the one that accepts
/// them: a C++ project's `compile_args` carry `-std=c++17`, which clang's C
/// driver rejects. Dependency-generation flags are stripped first -- see
/// [`marker_compile_args`] -- or the marker compile becomes the last writer of
/// the user's dependency file.
///
/// Compiled rather than synthesised with the `object` crate on purpose: a
/// synthesised Mach-O drops the platform load command, which makes the linker
/// warn about every object rllvm touches. Compiled rather than assembled from
/// a `.s` because that would need a section directive per object format.
pub(crate) fn build_marker_object(
    bitcode: &Path,
    dir: &Path,
    compiler: &Path,
    kind: CompilerKind,
    compile_args: &[String],
) -> Result<PathBuf, Error> {
    // The extension picks the language, so a C++ compiler is not asked to
    // treat a `.c` file as C++ -- which it does, but deprecated and with a
    // warning on every link.
    let extension = match kind {
        CompilerKind::Clang => "c",
        CompilerKind::ClangXX => "cpp",
    };
    let source = dir.join(format!("rllvm_marker.{extension}"));
    // A translation unit may not be empty, and the declaration must not
    // define a symbol that could collide at link time.
    fs::write(
        &source,
        b"typedef int rllvm_marker_empty_translation_unit;\n",
    )?;

    let marker = dir.join("rllvm_marker.o");
    let status = Command::new(compiler)
        .args(marker_compile_args(compile_args))
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&marker)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to build the rllvm marker object with {compiler:?}: exit_status={status}"
        )));
    }

    embed_bitcode_filepath_to_object_file::<&Path>(bitcode, &marker, None)?;

    Ok(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::utils::extract_bitcode_filepaths_from_object_file;

    /// A placeholder bitcode file. Embedding canonicalizes the path, so the
    /// file has to exist, but nothing ever reads its contents.
    fn placeholder_bitcode(dir: &Path) -> PathBuf {
        let bitcode = dir.join("crate.bc");
        fs::write(&bitcode, b"placeholder").expect("failed to write the placeholder bitcode");
        bitcode
    }

    #[test]
    fn marker_object_carries_the_bitcode_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bitcode = placeholder_bitcode(tmp.path());

        let clang = crate::config::try_rllvm_config()
            .expect("configuration")
            .clang_filepath()
            .clone();
        let marker = build_marker_object(&bitcode, tmp.path(), &clang, CompilerKind::Clang, &[])
            .expect("marker built");
        let paths =
            extract_bitcode_filepaths_from_object_file(&marker).expect("marker carries a section");

        // An already-absolute path is recorded verbatim, not canonicalized.
        assert_eq!(paths, vec![bitcode]);
    }
}
