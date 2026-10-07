//! Archive patching for the rustc wrapper.
//!
//! rustc archives rlibs and staticlibs itself, so there is no link to
//! intercept. rllvm embeds the bitcode path into each of the crate's own
//! members of the finished archive so that whichever one the linker pulls in
//! contributes the whole crate's bitcode path. A staticlib also bundles the
//! objects of every dependency and of the prebuilt sysroot; those are not
//! compiled from this crate's bitcode and keep whatever they already record.
//! Neither is rustc's allocator shim, which is named after the crate but
//! generated outside its bitcode, so a member that defines globals is patched
//! only when it defines something the bitcode defines.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use object::{Object, ObjectSymbol};

use rllvm_core::{
    config::try_rllvm_config,
    error::Error,
    utils::{embed_bitcode_filepath_to_object_file, execute_llvm_config},
};

/// `llvm-nm`'s file name. It has no configuration key of its own.
const LLVM_NM: &str = "llvm-nm";

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

/// The `llvm-nm` to read the crate's bitcode with: the one beside the
/// configured `llvm-ar`, or else the one in the bindir the configured
/// `llvm-config` reports. `llvm-ar` alone does not locate it: a wrapper
/// around the real `llvm-ar` sits in a directory with no other LLVM tool.
fn find_llvm_nm(llvm_ar: &Path, llvm_config: &Path) -> Result<PathBuf, Error> {
    let beside = llvm_ar.with_file_name(LLVM_NM);
    if beside.is_file() {
        return Ok(beside);
    }
    let bindir = execute_llvm_config(llvm_config, &["--bindir"]).unwrap_or_default();
    // An empty answer would otherwise name a file in the working directory.
    if !bindir.is_empty() {
        let in_bindir = Path::new(&bindir).join(LLVM_NM);
        if in_bindir.is_file() {
            return Ok(in_bindir);
        }
    }
    Err(Error::MissingFile(format!(
        "`{LLVM_NM}` is needed to patch a Rust archive, and is neither beside the configured \
         llvm-ar, at {}, nor in the bindir {bindir:?} of the configured llvm-config, {}",
        beside.display(),
        llvm_config.display()
    )))
}

/// The symbols the crate's bitcode module defines, read with `llvm_nm`.
/// Spelled as the target spells them, the way an object member's symbol
/// table does.
fn bitcode_definitions(llvm_nm: &Path, bitcode: &Path) -> Result<HashSet<String>, Error> {
    let output = Command::new(llvm_nm)
        .args(["--defined-only", "--just-symbol-name"])
        .arg(bitcode)
        .output()?;
    if !output.status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to list the symbols of {bitcode:?} with {llvm_nm:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Whether the object `data` was compiled from the module defining
/// `definitions`: it defines one of them, or no global symbol at all, as a
/// codegen unit of a crate with only generic code does. rustc's allocator
/// shim defines `__rust_alloc` and its siblings, globals no crate's bitcode
/// defines, so it is neither.
fn compiled_from(data: &[u8], definitions: &HashSet<String>) -> bool {
    let Ok(object) = object::File::parse(data) else {
        return false;
    };
    let mut defines_global = false;
    for symbol in object.symbols().filter(|symbol| symbol.is_definition()) {
        if symbol.name().is_ok_and(|name| definitions.contains(name)) {
            return true;
        }
        defines_global |= symbol.is_global();
    }
    !defines_global
}

/// Embed the bitcode path into each of the crate's own object members.
///
/// Every own member carries the same crate-level path, which is correct: the
/// `.bc` is per crate, not per codegen unit, so whichever member the linker
/// pulls in contributes the whole crate. Members outside `prefixes` -- a
/// dependency's objects, or the sysroot's prebuilt `std` and
/// `compiler_builtins` -- are left alone: their code is not in this crate's
/// bitcode, and a member that records nothing is how extraction learns a
/// part of the archive has no bitcode. So is a member named after the crate
/// whose globals the bitcode does not define, such as the allocator shim.
///
/// Returns the number of members patched.
pub(crate) fn patch_archive(
    archive: &Path,
    bitcode: &Path,
    prefixes: &[String],
) -> Result<usize, Error> {
    let config = try_rllvm_config()?;
    let llvm_ar = config.llvm_ar_filepath().clone();
    let llvm_nm = find_llvm_nm(&llvm_ar, config.llvm_config_filepath())?;
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

    let definitions = bitcode_definitions(&llvm_nm, bitcode)?;

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
        if !compiled_from(&data, &definitions) {
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

    /// The crate's bitcode module: the code of fixture member `index`, the
    /// way rustc's bitcode holds the code of the crate's own codegen units.
    fn crate_bitcode(dir: &Path, index: usize) -> PathBuf {
        let clang = pin_inferred_config()
            .expect("no usable LLVM configuration")
            .clang_filepath()
            .clone();
        let bitcode = dir.join("crate.bc");
        let status = Command::new(clang)
            .args(["-c", "-emit-llvm"])
            .arg(dir.join(format!("member{index}.c")))
            .arg("-o")
            .arg(&bitcode)
            .status()
            .expect("failed to run clang");
        assert!(status.success(), "compiling the crate bitcode failed");
        bitcode
    }

    /// A fixture member defining one function, named after its position.
    fn defining(index: usize) -> String {
        format!("int member{index}(void) {{ return {index}; }}\n")
    }

    /// An archive shaped like a staticlib: object members named as rustc
    /// names them, each compiled from its C source, plus one that is not an
    /// object, which must be left alone.
    fn build_fixture_archive(dir: &Path, objects: &[(&str, String)]) -> PathBuf {
        // Inferred from LLVM, never the user's config: `rllvm-core` resolves
        // the configuration once per process, and this is the only test here
        // that reads it.
        let config = pin_inferred_config().expect("no usable LLVM configuration");
        let clang = config.clang_filepath().clone();
        let llvm_ar = config.llvm_ar_filepath().clone();

        let mut members = Vec::new();
        for (index, (name, code)) in objects.iter().enumerate() {
            let source = dir.join(format!("member{index}.c"));
            fs::write(&source, code).expect("failed to write a fixture source");
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
        let own = ["fixture.fixture.2e58d64bd285cc71-cgu.0.rcgu.o"];
        // rustc's allocator shim: named after the crate, but generated beside
        // its codegen units, so none of its code is in the crate's bitcode.
        let shim = "fixture.awrgbl1ahkncdry3idj4cpuvm.rcgu.o";
        let foreign = "compiler_builtins-51dc6f60309b0c2f.compiler_builtins.1a788e7-cgu.000.rcgu.o";
        let archive = build_fixture_archive(
            tmp.path(),
            &[
                (own[0], defining(0)),
                (shim, defining(1)),
                (foreign, defining(2)),
            ],
        );
        let bitcode = crate_bitcode(tmp.path(), 0);

        let patched = patch_archive(&archive, &bitcode, &member_prefixes("fixture", "", None))
            .expect("patched");
        assert_eq!(
            patched, 1,
            "only members compiled from the crate's bitcode are patched"
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
    fn a_codegen_unit_defining_nothing_keeps_the_crate_path() {
        // A crate of only generic code compiles to a codegen unit and a
        // bitcode module that both define nothing. The member claims no code
        // the module lacks, so it records the crate like any other unit
        // rather than reading as a member without bitcode.
        let tmp = tempfile::tempdir().unwrap();
        let own = "generic-0a1b2c3d4e5f6071.generic.57163820d3278ffa-cgu.0.rcgu.o";
        let archive = build_fixture_archive(
            tmp.path(),
            &[(own, "static int unused(void) { return 0; }\n".to_string())],
        );
        let bitcode = crate_bitcode(tmp.path(), 0);

        let prefixes = member_prefixes("generic", "-0a1b2c3d4e5f6071", None);
        let patched = patch_archive(&archive, &bitcode, &prefixes).expect("patched");
        assert_eq!(
            patched, 1,
            "the empty codegen unit is still the crate's own"
        );
        let recorded = recorded_paths(&archive);
        assert_eq!(recorded, vec![(own.to_string(), vec![bitcode])]);
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
