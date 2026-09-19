//! Archive patching for the rustc wrapper.
//!
//! rustc archives rlibs and staticlibs itself, so there is no link to
//! intercept. rllvm embeds the bitcode path into every member of the finished
//! archive so that whichever member the linker pulls in contributes the whole
//! crate's bitcode path.

use std::{fs, path::Path, process::Command};

use crate::{config::try_rllvm_config, error::Error, utils::embed_bitcode_filepath_to_object_file};

/// Embed the bitcode path into every object member of an archive.
///
/// rustc archives rlibs and staticlibs itself, so there is no link to
/// intercept. Every member carries the same crate-level path, which is
/// correct: the `.bc` is per crate, not per codegen unit, so whichever member
/// the linker pulls in contributes the whole crate.
///
/// Returns the number of members patched.
pub(crate) fn patch_archive(archive: &Path, bitcode: &Path) -> Result<usize, Error> {
    let llvm_ar = try_rllvm_config()?.llvm_ar_filepath().clone();
    let archive = archive.canonicalize()?;

    let workspace = tempfile::tempdir()?;
    let status = Command::new(&llvm_ar)
        .arg("x")
        .arg(&archive)
        .current_dir(workspace.path())
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to unpack {archive:?} with {llvm_ar:?}: exit_status={status}"
        )));
    }

    let mut patched = Vec::new();
    for entry in fs::read_dir(workspace.path())? {
        let member = entry?.path();
        // An rlib carries `lib.rmeta` and `lib.rmeta-link` beside its objects.
        let data = fs::read(&member)?;
        if object::File::parse(&*data).is_err() {
            continue;
        }
        embed_bitcode_filepath_to_object_file::<&Path>(bitcode, &member, None)?;
        patched.push(member);
    }

    if patched.is_empty() {
        tracing::debug!("No object members to patch in {archive:?}");
        return Ok(0);
    }

    // `r` replaces the named members and leaves every other one, and its
    // ordering, alone -- rustc still has to be able to read the rlib.
    let status = Command::new(&llvm_ar)
        .arg("r")
        .arg(&archive)
        .args(&patched)
        .status()?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to repack {archive:?} with {llvm_ar:?}: exit_status={status}"
        )));
    }

    Ok(patched.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::utils::extract_bitcode_filepaths_from_parsed_objects;

    /// A placeholder bitcode file. Embedding canonicalizes the path, so the
    /// file has to exist, but nothing ever reads its contents.
    fn placeholder_bitcode(dir: &Path) -> PathBuf {
        let bitcode = dir.join("crate.bc");
        fs::write(&bitcode, b"placeholder").expect("failed to write the placeholder bitcode");
        bitcode
    }

    /// An archive shaped like an rlib: object members plus one that is not an
    /// object, which must be left alone.
    fn build_fixture_archive(dir: &Path, objects: usize) -> PathBuf {
        let config = try_rllvm_config().expect("no usable LLVM configuration");
        let clang = config.clang_filepath().clone();
        let llvm_ar = config.llvm_ar_filepath().clone();

        let mut members = Vec::new();
        for index in 0..objects {
            let source = dir.join(format!("member{index}.c"));
            fs::write(
                &source,
                format!("int member{index}(void) {{ return {index}; }}\n"),
            )
            .expect("failed to write a fixture source");
            let object = dir.join(format!("member{index}.o"));
            let status = Command::new(&clang)
                .arg("-c")
                .arg(&source)
                .arg("-o")
                .arg(&object)
                .status()
                .expect("failed to run clang");
            assert!(status.success(), "compiling the fixture member failed");
            members.push(object);
        }

        let metadata = dir.join("lib.rmeta");
        fs::write(&metadata, b"not an object file\n")
            .expect("failed to write the fixture metadata");
        members.push(metadata);

        let archive = dir.join("libfixture.rlib");
        let status = Command::new(&llvm_ar)
            .arg("r")
            .arg(&archive)
            .args(&members)
            .status()
            .expect("failed to run llvm-ar");
        assert!(status.success(), "packing the fixture archive failed");

        archive
    }

    #[test]
    fn patching_an_archive_reaches_every_object_member() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = build_fixture_archive(tmp.path(), 2);
        let bitcode = placeholder_bitcode(tmp.path());

        let patched = patch_archive(&archive, &bitcode).expect("archive patched");
        assert_eq!(
            patched, 2,
            "object members must be patched and non-object members skipped"
        );

        // There is no archive-level extract helper: `rllvm-get-bc` parses the
        // archive and feeds its members to the parsed-objects helper.
        let data = fs::read(&archive).unwrap();
        let parsed = object::read::archive::ArchiveFile::parse(&*data).expect("archive parses");
        let members: Vec<object::File> = parsed
            .members()
            .filter_map(Result::ok)
            .filter_map(|member| member.data(&*data).ok())
            .filter_map(|member| object::File::parse(member).ok())
            .collect();
        let paths = extract_bitcode_filepaths_from_parsed_objects(&members)
            .expect("members carry sections");

        // Both members carry the same crate-level path, deduplicated on read.
        assert_eq!(paths, vec![bitcode]);
    }
}
