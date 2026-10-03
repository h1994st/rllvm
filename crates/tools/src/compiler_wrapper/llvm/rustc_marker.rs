//! Archive patching for the rustc wrapper.
//!
//! rustc archives rlibs and staticlibs itself, so there is no link to
//! intercept. rllvm embeds the bitcode path into each of the crate's own
//! members of the finished archive so that whichever one the linker pulls in
//! contributes the whole crate's bitcode path. A staticlib also bundles the
//! objects of every dependency and of the prebuilt sysroot; those are not
//! compiled from this crate's bitcode and keep whatever they already record.

use std::{fs, path::Path, process::Command};

use rllvm_core::{
    config::try_rllvm_config, error::Error, utils::embed_bitcode_filepath_to_object_file,
};

/// The member-name prefixes rustc gives a crate's own objects. rustc names
/// each codegen unit after the output's file stem: `-o libfoo.a` gives
/// `libfoo.`, and Cargo's `--out-dir` gives `{crate}{extra-filename}.` inside
/// an rlib and `{crate}.` for a staticlib or cdylib. Every other member of a
/// staticlib comes from a dependency or the sysroot and is named after that
/// crate instead.
pub(crate) fn member_prefixes(
    crate_name: &str,
    extra_filename: &str,
    output_stem: Option<&str>,
) -> Vec<String> {
    let mut prefixes = vec![format!("{crate_name}.")];
    if !extra_filename.is_empty() {
        prefixes.push(format!("{crate_name}{extra_filename}."));
    }
    if let Some(stem) = output_stem {
        prefixes.push(format!("{stem}."));
    }
    prefixes
}

/// Whether an archive member named `member` is one of the crate's own.
fn owns(prefixes: &[String], member: &str) -> bool {
    prefixes
        .iter()
        .any(|prefix| member.starts_with(prefix.as_str()))
}

/// Embed the bitcode path into each of the crate's own object members.
///
/// Every own member carries the same crate-level path, which is correct: the
/// `.bc` is per crate, not per codegen unit, so whichever member the linker
/// pulls in contributes the whole crate. Members outside `prefixes` -- a
/// dependency's objects, or the sysroot's prebuilt `std` and
/// `compiler_builtins` -- are left alone: their code is not in this crate's
/// bitcode, and a member that records nothing is how extraction learns a
/// part of the archive has no bitcode.
///
/// Returns the number of members patched.
pub(crate) fn patch_archive(
    archive: &Path,
    bitcode: &Path,
    prefixes: &[String],
) -> Result<usize, Error> {
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
        let owned = member
            .file_name()
            .is_some_and(|name| owns(prefixes, &name.to_string_lossy()));
        if !owned {
            continue;
        }
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

    use rllvm_core::{
        config::pin_inferred_config, utils::extract_bitcode_filepaths_from_parsed_objects,
    };

    /// A placeholder bitcode file. Embedding canonicalizes the path, so the
    /// file has to exist, but nothing ever reads its contents.
    fn placeholder_bitcode(dir: &Path) -> PathBuf {
        let bitcode = dir.join("crate.bc");
        fs::write(&bitcode, b"placeholder").expect("failed to write the placeholder bitcode");
        bitcode
    }

    /// An archive shaped like a staticlib: object members named as rustc
    /// names them, plus one that is not an object, which must be left alone.
    fn build_fixture_archive(dir: &Path, objects: &[&str]) -> PathBuf {
        // Inferred from LLVM, never the user's config: `rllvm-core` resolves
        // the configuration once per process, and this is the only test here
        // that reads it.
        let config = pin_inferred_config().expect("no usable LLVM configuration");
        let clang = config.clang_filepath().clone();
        let llvm_ar = config.llvm_ar_filepath().clone();

        let mut members = Vec::new();
        for (index, name) in objects.iter().enumerate() {
            let source = dir.join(format!("member{index}.c"));
            fs::write(
                &source,
                format!("int member{index}(void) {{ return {index}; }}\n"),
            )
            .expect("failed to write a fixture source");
            let object = dir.join(name);
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

        let archive = dir.join("libfixture.a");
        let status = Command::new(&llvm_ar)
            .arg("r")
            .arg(&archive)
            .args(&members)
            .status()
            .expect("failed to run llvm-ar");
        assert!(status.success(), "packing the fixture archive failed");

        archive
    }

    /// The bitcode paths each object member of `archive` records, by name.
    fn recorded_paths(archive: &Path) -> Vec<(String, Vec<PathBuf>)> {
        // There is no archive-level extract helper: `rllvm-get-bc` parses the
        // archive and feeds its members to the parsed-objects helper.
        let data = fs::read(archive).unwrap();
        let parsed = object::read::archive::ArchiveFile::parse(&*data).expect("archive parses");
        parsed
            .members()
            .filter_map(Result::ok)
            .filter_map(|member| {
                let name = String::from_utf8_lossy(member.name()).into_owned();
                let object = object::File::parse(member.data(&*data).ok()?).ok()?;
                let paths = extract_bitcode_filepaths_from_parsed_objects(&[object]).unwrap();
                Some((name, paths))
            })
            .collect()
    }

    #[test]
    fn patching_an_archive_reaches_only_the_crates_own_members() {
        // A staticlib bundles the sysroot's prebuilt objects beside the
        // crate's own codegen units. Only the crate's own members are
        // compiled from the crate's bitcode; stamping its path on the others
        // would claim a module that does not hold their code.
        let tmp = tempfile::tempdir().unwrap();
        let own = [
            "fixture.fixture.2e58d64bd285cc71-cgu.0.rcgu.o",
            "fixture.awrgbl1ahkncdry3idj4cpuvm.rcgu.o",
        ];
        let foreign = "compiler_builtins-51dc6f60309b0c2f.compiler_builtins.1a788e7-cgu.000.rcgu.o";
        let archive = build_fixture_archive(tmp.path(), &[own[0], own[1], foreign]);
        let bitcode = placeholder_bitcode(tmp.path());

        let patched = patch_archive(&archive, &bitcode, &member_prefixes("fixture", "", None))
            .expect("patched");
        assert_eq!(
            patched, 2,
            "only the crate's own object members are patched"
        );

        for (name, paths) in recorded_paths(&archive) {
            if own.contains(&name.as_str()) {
                assert_eq!(
                    paths,
                    vec![bitcode.clone()],
                    "{name} records the crate's bitcode"
                );
            } else {
                assert!(
                    paths.is_empty(),
                    "{name} must not claim the crate's bitcode"
                );
            }
        }
    }

    #[test]
    fn own_members_are_named_after_the_output() {
        let cargo_rlib = member_prefixes("boring", "-937d58ce64b0d897", None);
        assert!(owns(
            &cargo_rlib,
            "boring-937d58ce64b0d897.boring.b487-cgu.3.rcgu.o"
        ));
        assert!(!owns(
            &cargo_rlib,
            "boring_sys-12.boring_sys.x-cgu.0.rcgu.o"
        ));
        let cargo_staticlib = member_prefixes("quiche", "-1a2b", None);
        assert!(owns(&cargo_staticlib, "quiche.quiche.2e58-cgu.0.rcgu.o"));
        assert!(!owns(
            &cargo_staticlib,
            "quiche_extra-99.quiche_extra.x-cgu.0.rcgu.o"
        ));
        // `rustc --crate-type staticlib rust_side.rs -o librustside.a`
        let direct = member_prefixes("rust_side", "", Some("librustside"));
        assert!(owns(&direct, "librustside.rust_side.3e35-cgu.0.rcgu.o"));
        assert!(!owns(
            &direct,
            "compiler_builtins-5.compiler_builtins.1-cgu.000.rcgu.o"
        ));
    }
}
