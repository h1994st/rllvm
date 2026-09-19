//! A marker object carrying one bitcode path.
//!
//! Two callers need the same thing — an object that contributes a bitcode path
//! to a link it is added to, and nothing else. The rustc wrapper passes it as
//! `-C link-arg`; a `save-temps` LTO link passes it as an ordinary input so
//! the finished binary names the module the linker saved.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    arg_parser::without_dependency_flags,
    compiler_wrapper::CompilerKind,
    constants::ELF_SECTION_NAME,
    error::Error,
    lto::escape_for_assembler,
    utils::{embed_bitcode_filepath_to_object_file, execute_llvm_tool, recorded_bitcode_filepath},
};

/// Compile a marker translation unit carrying the bitcode path.
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
/// [`without_dependency_flags`] -- or the marker compile becomes the last writer of
/// the user's dependency file.
///
/// Compiled rather than synthesised with the `object` crate on purpose: a
/// synthesised Mach-O drops the platform load command, which makes the linker
/// warn about every object rllvm touches. ELF uses module assembly to retain
/// its otherwise unreferenced metadata; other formats use ordinary embedding.
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
    // GNU ld can collect even nonallocated sections when an object has no
    // retained allocated section, as with this standalone marker. Let the
    // target assembler set SHF_GNU_RETAIN and the matching ELF OSABI together:
    // setting the flag alone on a System V object does not retain it in BFD.
    // Other formats keep the typedef-only unit and ordinary embedding below.
    let path = escape_for_assembler(&recorded_bitcode_filepath(bitcode)?);
    fs::write(
        &source,
        format!(
            r#"#if defined(__ELF__)
__asm__(".section {ELF_SECTION_NAME},\"R\"\n.ascii \"{path}\\n\"\n.previous");
#else
typedef int rllvm_marker_empty_translation_unit;
#endif
"#
        ),
    )?;

    let marker = dir.join("rllvm_marker.o");
    let mut args: Vec<OsString> = without_dependency_flags(compile_args)
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend([
        OsString::from("-c"),
        source.into_os_string(),
        OsString::from("-o"),
        marker.as_os_str().to_owned(),
    ]);
    let status = execute_llvm_tool(compiler, &args)?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to build the rllvm marker object with {compiler:?}: exit_status={status}"
        )));
    }

    let data = fs::read(&marker)?;
    if object::File::parse(&*data)?.format() != object::BinaryFormat::Elf {
        embed_bitcode_filepath_to_object_file::<&Path>(bitcode, &marker, None)?;
    }

    Ok(marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::utils::extract_bitcode_filepaths_from_object_file;

    /// Nothing reads the placeholder's contents; its name exercises both
    /// layers of escaping in the generated ELF marker assembly.
    fn placeholder_bitcode(dir: &Path) -> PathBuf {
        let bitcode = dir.join("crate\\with\"quote.bc");
        fs::write(&bitcode, b"placeholder").expect("failed to write the placeholder bitcode");
        bitcode
    }

    #[test]
    fn marker_object_carries_path_with_backslash_and_quote() {
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

    #[test]
    fn elf_marker_uses_requested_arm_target() {
        use object::{Object, ObjectSection};

        let tmp = tempfile::tempdir().unwrap();
        let bitcode = placeholder_bitcode(tmp.path());
        let clang = crate::config::try_rllvm_config()
            .expect("configuration")
            .clang_filepath();
        let marker = build_marker_object(
            &bitcode,
            tmp.path(),
            clang,
            CompilerKind::Clang,
            &["--target=armv7-linux-gnueabihf".into()],
        )
        .expect("ARM marker built without linking or a sysroot");
        let data = fs::read(&marker).unwrap();
        let object = object::File::parse(&*data).unwrap();
        assert_eq!(object.architecture(), object::Architecture::Arm);
        let section = object.section_by_name(ELF_SECTION_NAME).unwrap();
        assert_eq!(
            section.flags(),
            object::SectionFlags::Elf {
                sh_type: object::elf::SHT_PROGBITS,
                sh_flags: object::elf::SHF_GNU_RETAIN,
            }
        );
        assert_eq!(
            section.data().unwrap(),
            format!("{}\n", bitcode.display()).as_bytes()
        );
    }
}
