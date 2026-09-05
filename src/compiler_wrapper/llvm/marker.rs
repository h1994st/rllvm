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

use crate::{config::try_rllvm_config, error::Error, utils::embed_bitcode_filepath_to_object_file};

/// Compile an empty translation unit and embed the bitcode path into it.
///
/// The object goes to rustc as `-C link-arg`, so the linker concatenates its
/// section along with every other object's, and `rllvm-get-bc` finds the path
/// in the finished binary.
///
/// Compiled rather than synthesised with the `object` crate on purpose: a
/// synthesised Mach-O drops the platform load command, which makes the linker
/// warn about every object rllvm touches. Compiled rather than assembled from
/// a `.s` because that would need a section directive per object format.
pub(crate) fn build_marker_object(bitcode: &Path, dir: &Path) -> Result<PathBuf, Error> {
    let source = dir.join("rllvm_marker.c");
    // A C translation unit may not be empty, and the declaration must not
    // define a symbol that could collide at link time.
    fs::write(
        &source,
        b"typedef int rllvm_marker_empty_translation_unit;\n",
    )?;

    let marker = dir.join("rllvm_marker.o");
    let clang = try_rllvm_config()?.clang_filepath().clone();
    let status = Command::new(&clang)
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&marker)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to build the rllvm marker object with {clang:?}: exit_status={status}"
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

        let marker = build_marker_object(&bitcode, tmp.path()).expect("marker built");
        let paths =
            extract_bitcode_filepaths_from_object_file(&marker).expect("marker carries a section");

        // An already-absolute path is recorded verbatim, not canonicalized.
        assert_eq!(paths, vec![bitcode]);
    }
}
