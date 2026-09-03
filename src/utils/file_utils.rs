//! File-related, especially object-file-related, utility functions

use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process, str,
    sync::atomic::{AtomicU64, Ordering},
};

use object::{
    BinaryFormat, File, Object, ObjectComdat, ObjectKind, ObjectSection, ObjectSymbol,
    RelocationTarget, SectionFlags, SectionKind, SymbolFlags, SymbolSection, write,
};

use crate::{
    config::try_rllvm_config,
    constants::{
        COFF_SECTION_NAME, DARWIN_SECTION_NAME, DARWIN_SEGMENT_NAME, ELF_SECTION_NAME,
        WASM_SECTION_NAME,
    },
    error::Error,
    utils::execute_command_for_status,
};

/// Returns `true` if the path exists and is not a directory.
pub(crate) fn is_plain_file<P>(file: P) -> bool
where
    P: AsRef<Path>,
{
    let file = file.as_ref();
    file.exists() && !file.is_dir()
}

/// Returns `true` if the file is a relocatable object file.
pub(crate) fn is_object_file<P>(file: P) -> Result<bool, Error>
where
    P: AsRef<Path>,
{
    let file = file.as_ref();

    if !is_plain_file(file) {
        return Ok(false);
    }

    let data = fs::read(file)?;

    // A file that does not parse as an object simply is not one. Propagating
    // the parse error here would abort the whole invocation, because the
    // argument parser calls this to classify every argument it does not
    // otherwise recognize — an Objective-C source, a linker script, or any
    // other unrecognized-but-existing file would take the build down with it.
    match object::File::parse(&*data) {
        Ok(object_file) => Ok(object_file.kind() == ObjectKind::Relocatable),
        Err(err) => {
            tracing::debug!("Not an object file: file={:?}, err={}", file, err);
            Ok(false)
        }
    }
}

/// Resolve the bitcode filepath to a string for embedding.
fn resolve_bitcode_filepath(bitcode_filepath: &Path) -> Result<String, Error> {
    let absolute_filepath = if bitcode_filepath.is_absolute() {
        bitcode_filepath.to_path_buf()
    } else {
        bitcode_filepath.canonicalize()?
    };

    // When a bitcode root is configured, record the path relative to it. An
    // absolute path pins the object to the machine and directory that built it,
    // so it breaks under `mv`, container extraction, compiler caches replaying
    // an object into a different tree, and CI artifacts consumed by another job.
    // A relative entry survives all of those; `rllvm-get-bc --bitcode-root`
    // supplies the root again at extraction time.
    //
    // Unset is the default and keeps the historical absolute form, so objects
    // produced by older versions stay readable. The reader distinguishes the two
    // by the leading separator, which is why no format flag is needed.
    let recorded = try_rllvm_config()
        .ok()
        .and_then(|config| config.bitcode_root())
        .and_then(|root| {
            absolute_filepath
                .strip_prefix(&root)
                .ok()
                .map(|relative| relative.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| absolute_filepath.to_string_lossy().into_owned());

    // The linker concatenates these sections when it merges object files, so
    // every entry must be newline-terminated for the reader to split the
    // combined section back into individual paths.
    Ok(format!("{recorded}\n"))
}

/// Encode an unsigned integer as a LEB128 byte sequence.
fn encode_leb128(mut value: usize) -> Vec<u8> {
    let mut result = vec![];
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        result.push(byte);
        if value == 0 {
            break;
        }
    }
    result
}

/// Append a custom section to a WASM binary.
///
/// WASM custom sections have the format:
/// - Section ID: 0 (custom section)
/// - Section size (LEB128)
/// - Name length (LEB128)
/// - Name bytes
/// - Section payload
fn append_wasm_custom_section(wasm_data: &[u8], section_name: &str, payload: &[u8]) -> Vec<u8> {
    let name_bytes = section_name.as_bytes();
    let name_len_encoded = encode_leb128(name_bytes.len());
    let content_size = name_len_encoded.len() + name_bytes.len() + payload.len();
    let section_size_encoded = encode_leb128(content_size);

    let mut result = wasm_data.to_vec();
    result.push(0x00); // Custom section ID
    result.extend_from_slice(&section_size_encoded);
    result.extend_from_slice(&name_len_encoded);
    result.extend_from_slice(name_bytes);
    result.extend_from_slice(payload);
    result
}

/// Embed the path of the bitcode to the corresponding object file
pub fn embed_bitcode_filepath_to_object_file<P>(
    bitcode_filepath: P,
    object_filepath: P,
    output_object_filepath: Option<P>,
) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    let bitcode_filepath = bitcode_filepath.as_ref();
    let object_filepath = object_filepath.as_ref();

    let data = fs::read(object_filepath)?;
    let object_file = object::File::parse(&*data)?;
    let object_binary_format = object_file.format();

    let bitcode_filepath_string = resolve_bitcode_filepath(bitcode_filepath)?;

    // Prefer `llvm-objcopy` where it is configured and the format supports it.
    // Rebuilding the object with the `object` crate writer (below) loses
    // information the writer does not model — on Mach-O it drops the platform
    // load command, which makes the linker emit
    // `no platform load command found in '...', assuming: macOS` for every
    // object rllvm touches. `llvm-objcopy` rewrites in place and keeps it.
    if !matches!(object_binary_format, BinaryFormat::Wasm)
        && let Some(objcopy_filepath) = try_rllvm_config()?.llvm_objcopy_filepath()
        && objcopy_filepath.exists()
    {
        return embed_with_objcopy(
            objcopy_filepath,
            object_binary_format,
            &bitcode_filepath_string,
            object_filepath,
            output_object_filepath.as_ref().map(|p| p.as_ref()),
        );
    }

    // Failing that, the read-modify-write builder keeps unmodelled load
    // commands where the rebuild below would drop them. It only covers Mach-O,
    // and only objects whose load commands it understands.
    if object_binary_format == BinaryFormat::MachO
        && let Some(output_data) = embed_with_macho_builder(&data, &bitcode_filepath_string)
    {
        return write_object_output(output_data, object_filepath, output_object_filepath);
    }

    let output_data = match object_binary_format {
        BinaryFormat::Wasm => {
            // The `object` crate's write API does not support WASM, so we
            // directly append a custom section to the raw binary.
            append_wasm_custom_section(&data, WASM_SECTION_NAME, bitcode_filepath_string.as_bytes())
        }
        _ => {
            // Platform-dependent properties
            let (segment_name, section_name, flags) = match object_binary_format {
                BinaryFormat::Elf => (
                    vec![],
                    ELF_SECTION_NAME.as_bytes().to_vec(),
                    SectionFlags::Elf {
                        sh_type: object::elf::SHT_PROGBITS,
                        sh_flags: object::elf::SectionFlags(0),
                    },
                ),
                BinaryFormat::MachO => (
                    DARWIN_SEGMENT_NAME.as_bytes().to_vec(),
                    DARWIN_SECTION_NAME.as_bytes().to_vec(),
                    SectionFlags::MachO {
                        flags: object::macho::SectionFlags(0),
                        reserved2: 0,
                    },
                ),
                BinaryFormat::Coff => (
                    vec![],
                    COFF_SECTION_NAME.as_bytes().to_vec(),
                    SectionFlags::Coff {
                        characteristics: object::pe::SectionFlags(0),
                    },
                ),
                _ => {
                    return Err(Error::UnsupportedBinaryFormat(format!(
                        "{:?}",
                        object_binary_format
                    )));
                }
            };

            // Copy the input object file into a new mutable object file
            let mut new_object_file = copy_object_file(object_file)?;

            // Add a section
            let section_id =
                new_object_file.add_section(segment_name, section_name, SectionKind::Unknown);
            let new_section = new_object_file.section_mut(section_id);

            new_section.set_data(bitcode_filepath_string.as_bytes(), 1);
            // NOTE: we have to explicitly set flags; otherwise, the flags will be
            // inferred based on the section kind, but `Section::Unknown` is not
            // supported for auto inferring flags
            new_section.flags = flags;

            new_object_file.write()?
        }
    };

    write_object_output(output_data, object_filepath, output_object_filepath)
}

/// Write the rewritten object, either to the requested path or over the input.
fn write_object_output<P>(
    output_data: Vec<u8>,
    object_filepath: &Path,
    output_object_filepath: Option<P>,
) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    match output_object_filepath {
        Some(output_object_filepath) => fs::write(output_object_filepath, output_data)?,
        None => fs::write(object_filepath, output_data)?,
    }

    Ok(())
}

/// Pack a section or segment name into the fixed-size field Mach-O uses.
fn macho_name_field(name: &str) -> Option<[u8; 16]> {
    let bytes = name.as_bytes();
    if bytes.len() > 16 {
        return None;
    }
    let mut field = [0u8; 16];
    field[..bytes.len()].copy_from_slice(bytes);
    Some(field)
}

/// Embed the bitcode path by editing the Mach-O with the `object` crate's
/// read-modify-write builder.
///
/// Unlike [`copy_object_file`], which reconstructs the object from an abstract
/// model and drops anything outside it, the builder round-trips the input and
/// keeps unmodelled load commands as opaque bytes.
///
/// Returns `None` when the builder cannot represent the input, so the caller
/// falls back to rebuilding. It refuses objects carrying load commands it does
/// not model — `LC_LINKER_OPTION`, emitted for autolinking, is one such case —
/// which is a loud failure rather than a silent loss.
fn embed_with_macho_builder(data: &[u8], bitcode_filepath_string: &str) -> Option<Vec<u8>> {
    use object::build::macho::{Builder, SectionData};

    let mut builder = Builder::read(data)
        .inspect_err(|err| {
            tracing::debug!("Mach-O builder cannot handle this object: {}", err);
        })
        .ok()?;

    let sectname = macho_name_field(DARWIN_SECTION_NAME)?;
    let segname = macho_name_field(DARWIN_SEGMENT_NAME)?;

    let section_id = {
        let section = builder.sections.add();
        section.sectname = sectname;
        section.segname = segname;
        // Mach-O stores the alignment as a power of two, so 0 means
        // byte-aligned. Anything larger makes the linker pad between the
        // sections it concatenates, and those NUL bytes land in the middle of
        // the newline-separated path list.
        section.align = 0;
        section.data = SectionData::Data(bitcode_filepath_string.as_bytes().to_vec().into());
        section.id()
    };

    // A section is only written if a segment references it. A relocatable
    // object carries exactly one segment holding every section.
    let segment = builder.segments.iter_mut().next()?;
    segment.sections.push(section_id);

    let mut output_data = Vec::new();
    builder
        .write(&mut output_data)
        .inspect_err(|err| {
            tracing::debug!("Mach-O builder failed to write: {}", err);
        })
        .ok()?;

    Some(output_data)
}

/// Section specifier for `llvm-objcopy --add-section`.
///
/// Mach-O needs the segment as well; the other formats name the section alone.
fn objcopy_section_specifier(format: BinaryFormat) -> Result<String, Error> {
    match format {
        BinaryFormat::Elf => Ok(ELF_SECTION_NAME.to_string()),
        BinaryFormat::MachO => Ok(format!("{DARWIN_SEGMENT_NAME},{DARWIN_SECTION_NAME}")),
        BinaryFormat::Coff => Ok(COFF_SECTION_NAME.to_string()),
        _ => Err(Error::UnsupportedBinaryFormat(format!("{format:?}"))),
    }
}

/// Embed the bitcode path by shelling out to `llvm-objcopy --add-section`.
///
/// The payload has to reach objcopy as a file, so it is staged in a uniquely
/// named temporary file and removed afterwards.
fn embed_with_objcopy(
    objcopy_filepath: &Path,
    format: BinaryFormat,
    bitcode_filepath_string: &str,
    object_filepath: &Path,
    output_object_filepath: Option<&Path>,
) -> Result<(), Error> {
    static PAYLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

    let section_specifier = objcopy_section_specifier(format)?;

    let payload_filepath = env::temp_dir().join(format!(
        "rllvm-bcpath-{}-{}",
        process::id(),
        PAYLOAD_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&payload_filepath, bitcode_filepath_string)?;

    let mut args = vec![
        format!(
            "--add-section={section_specifier}={}",
            payload_filepath.display()
        ),
        object_filepath.to_string_lossy().into_owned(),
    ];
    if let Some(output_object_filepath) = output_object_filepath {
        args.push(output_object_filepath.to_string_lossy().into_owned());
    }

    let status = execute_command_for_status(objcopy_filepath, &args);

    // Remove the staged payload whether or not objcopy succeeded.
    let _ = fs::remove_file(&payload_filepath);

    let status = status?;
    if !status.success() {
        return Err(Error::ExecutionFailure(format!(
            "Failed to embed the bitcode path with {objcopy_filepath:?}: exit_status={status}"
        )));
    }

    Ok(())
}

/// Carry the Mach-O `LC_BUILD_VERSION` command across a rebuild.
///
/// [`copy_object_file`] reconstructs the object from the pieces the writer
/// models — sections, symbols, relocations, comdats — and anything outside that
/// set is silently dropped. `LC_BUILD_VERSION` is one such casualty, and losing
/// it makes the linker report `no platform load command found in '...',
/// assuming: macOS` for every object rllvm touches.
///
/// Only `LC_BUILD_VERSION` is restored. Objects from older toolchains carry
/// `LC_VERSION_MIN_MACOSX` instead, which the writer cannot emit.
fn copy_macho_build_version(in_object: &File, out_object: &mut write::Object) -> Result<(), Error> {
    let build_version = match in_object {
        File::MachO32(macho) => macho.build_version()?,
        File::MachO64(macho) => macho.build_version()?,
        _ => return Ok(()),
    };

    if let Some(build_version) = build_version {
        let endian = in_object.endianness();
        let mut version = write::MachOBuildVersion::default();
        let (build_version, _tools) = build_version;
        version.platform = build_version.platform.get(endian);
        version.minos = build_version.minos.get(endian);
        version.sdk = build_version.sdk.get(endian);
        out_object.set_macho_build_version(version);
    }

    Ok(())
}

fn copy_object_file(in_object: File) -> Result<write::Object, Error> {
    if in_object.kind() != ObjectKind::Relocatable {
        return Err(Error::InvalidArguments(format!(
            "Unsupported object kind: {:?}",
            in_object.kind()
        )));
    }

    let mut out_object = write::Object::new(
        in_object.format(),
        in_object.architecture(),
        in_object.endianness(),
    );
    out_object.mangling = write::Mangling::None;
    out_object.flags = in_object.flags();
    copy_macho_build_version(&in_object, &mut out_object)?;

    // Sections
    let mut out_sections = HashMap::new();
    for in_section in in_object.sections() {
        if in_section.kind() == SectionKind::Metadata {
            continue;
        }

        let section_id = out_object.add_section(
            in_section.segment_name()?.unwrap_or("").as_bytes().to_vec(),
            in_section.name()?.as_bytes().to_vec(),
            in_section.kind(),
        );
        let out_section = out_object.section_mut(section_id);
        if out_section.is_bss() {
            out_section.append_bss(in_section.size(), in_section.align());
        } else {
            out_section.set_data(in_section.data()?, in_section.align());
        }
        out_section.flags = in_section.flags();

        out_sections.insert(in_section.index(), section_id);
    }

    // Symbols
    let mut out_symbols = HashMap::new();
    for in_symbol in in_object.symbols() {
        let (section, value) = match in_symbol.section() {
            SymbolSection::None => (write::SymbolSection::None, in_symbol.address()),
            SymbolSection::Undefined => (write::SymbolSection::Undefined, in_symbol.address()),
            SymbolSection::Absolute => (write::SymbolSection::Absolute, in_symbol.address()),
            SymbolSection::Common => (write::SymbolSection::Common, in_symbol.address()),
            SymbolSection::Section(index) => {
                if let Some(out_section) = out_sections.get(&index) {
                    (
                        write::SymbolSection::Section(*out_section),
                        in_symbol.address() - in_object.section_by_index(index)?.address(),
                    )
                } else {
                    // Ignore symbols for sections that we have skipped
                    continue;
                }
            }
            _ => {
                return Err(Error::InvalidArguments(format!(
                    "Unknown symbol section: {:?}",
                    in_symbol
                )));
            }
        };
        let flags = match in_symbol.flags() {
            SymbolFlags::None => SymbolFlags::None,
            SymbolFlags::Elf { st_info, st_other } => SymbolFlags::Elf { st_info, st_other },
            SymbolFlags::MachO { n_type, n_desc } => SymbolFlags::MachO { n_type, n_desc },
            SymbolFlags::CoffSection {
                typ,
                storage_class,
                selection,
                associative_section,
            } => {
                let associative_section =
                    associative_section.map(|index| *out_sections.get(&index).unwrap());
                SymbolFlags::CoffSection {
                    typ,
                    storage_class,
                    selection,
                    associative_section,
                }
            }
            SymbolFlags::Xcoff {
                n_type,
                n_sclass,
                x_smtyp,
                x_smclas,
                containing_csect,
            } => {
                let containing_csect =
                    containing_csect.map(|index| *out_symbols.get(&index).unwrap());
                SymbolFlags::Xcoff {
                    n_type,
                    n_sclass,
                    x_smtyp,
                    x_smclas,
                    containing_csect,
                }
            }
            _ => {
                return Err(Error::InvalidArguments(format!(
                    "Unknown symbol flags: {:?}",
                    in_symbol
                )));
            }
        };
        let out_symbol = write::Symbol {
            name: in_symbol.name().unwrap_or("").as_bytes().to_vec(),
            value,
            size: in_symbol.size(),
            kind: in_symbol.kind(),
            scope: in_symbol.scope(),
            weak: in_symbol.is_weak(),
            section,
            flags,
        };
        let symbol_id = out_object.add_symbol(out_symbol);
        out_symbols.insert(in_symbol.index(), symbol_id);
    }

    // Relocations
    for in_section in in_object.sections() {
        if in_section.kind() == SectionKind::Metadata {
            continue;
        }

        let out_section = *out_sections.get(&in_section.index()).unwrap();
        for (offset, in_relocation) in in_section.relocations() {
            let symbol = match in_relocation.target() {
                RelocationTarget::Symbol(symbol) => *out_symbols.get(&symbol).unwrap(),
                RelocationTarget::Section(section) => {
                    out_object.section_symbol(*out_sections.get(&section).unwrap())
                }
                _ => {
                    return Err(Error::InvalidArguments(format!(
                        "Unknown relocation target: {:?}",
                        in_relocation
                    )));
                }
            };
            let out_relocation = write::Relocation {
                offset,
                symbol,
                addend: in_relocation.addend(),
                flags: in_relocation.flags(),
            };
            out_object.add_relocation(out_section, out_relocation)?;
        }
    }

    // Comdats
    for in_comdat in in_object.comdats() {
        let mut sections = vec![];
        for in_section in in_comdat.sections() {
            sections.push(*out_sections.get(&in_section).unwrap());
        }
        let out_comdat = write::Comdat {
            kind: in_comdat.kind(),
            symbol: *out_symbols.get(&in_comdat.symbol()).unwrap(),
            sections,
        };
        out_object.add_comdat(out_comdat);
    }

    Ok(out_object)
}

/// Extract bitcode filepaths embedded in a parsed object file.
pub fn extract_bitcode_filepaths_from_parsed_object(
    object_file: &object::File,
) -> Result<Vec<PathBuf>, Error> {
    let object_binary_format = object_file.format();

    let section_name = match object_binary_format {
        BinaryFormat::Elf => ELF_SECTION_NAME.as_bytes(),
        BinaryFormat::MachO => DARWIN_SECTION_NAME.as_bytes(),
        BinaryFormat::Coff => COFF_SECTION_NAME.as_bytes(),
        BinaryFormat::Wasm => WASM_SECTION_NAME.as_bytes(),
        _ => {
            return Err(Error::UnsupportedBinaryFormat(format!(
                "{:?}",
                object_binary_format
            )));
        }
    };

    match object_file.section_by_name_bytes(section_name) {
        Some(section) => {
            let section_data = section.data()?;
            let embedded_filepath_string = str::from_utf8(section_data)?.trim();

            let mut embedded_filepaths: Vec<_> = embedded_filepath_string
                .split('\n')
                .map(PathBuf::from)
                .collect();

            // Sort
            embedded_filepaths.sort();

            // Deduplicate
            embedded_filepaths.dedup();

            Ok(embedded_filepaths)
        }
        None => Ok(vec![]),
    }
}

/// Extract bitcode filepaths from an object file on disk.
pub fn extract_bitcode_filepaths_from_object_file<P>(
    object_filepath: P,
) -> Result<Vec<PathBuf>, Error>
where
    P: AsRef<Path>,
{
    let object_filepath = object_filepath.as_ref();

    let data = fs::read(object_filepath)?;
    let object_file = object::File::parse(&*data)?;

    extract_bitcode_filepaths_from_parsed_object(&object_file)
}

/// Extract and deduplicate bitcode filepaths from multiple parsed object files.
pub fn extract_bitcode_filepaths_from_parsed_objects(
    object_files: &[object::File],
) -> Result<Vec<PathBuf>, Error> {
    let mut bitcode_filepaths = vec![];
    for object_file in object_files {
        bitcode_filepaths.extend(extract_bitcode_filepaths_from_parsed_object(object_file)?);
    }

    // Sort
    bitcode_filepaths.sort();

    // Deduplicate
    bitcode_filepaths.dedup();

    Ok(bitcode_filepaths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// Builds an absolute path for a bitcode file used in embedding tests.
    ///
    /// The file need not exist, but the path must be absolute *on the host*:
    /// a relative path gets canonicalized, which fails when the file is
    /// missing. `/tmp/...` is not absolute on Windows.
    fn tmp_bitcode(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    /// Read the `LC_BUILD_VERSION` triple from a Mach-O object, if present.
    fn macho_build_version(object_file: &File) -> Option<(u32, u32, u32)> {
        let endian = object_file.endianness();
        let build_version = match object_file {
            File::MachO32(macho) => macho.build_version().ok()?,
            File::MachO64(macho) => macho.build_version().ok()?,
            _ => return None,
        }?;
        let (build_version, _tools) = build_version;
        Some((
            build_version.platform.get(endian).0,
            build_version.minos.get(endian).0,
            build_version.sdk.get(endian).0,
        ))
    }

    #[test]
    fn test_macho_builder_embeds_without_padding() {
        let data = create_minimal_macho_object();
        let before = object::File::parse(&*data).expect("Failed to parse the object");
        let expected_version = macho_build_version(&before);

        let payload = "/tmp/one.bc\n";
        let output = embed_with_macho_builder(&data, payload)
            .expect("the builder should handle a plain object");
        let after = object::File::parse(&*output).expect("Failed to parse the output");

        // The whole point of this path: load commands survive.
        assert_eq!(
            macho_build_version(&after),
            expected_version,
            "the builder must round-trip LC_BUILD_VERSION"
        );

        let section = after
            .section_by_name_bytes(DARWIN_SECTION_NAME.as_bytes())
            .expect("bitcode section missing");
        assert_eq!(
            section.data().expect("Failed to read the section"),
            payload.as_bytes()
        );

        // The section must be byte-aligned. Mach-O stores alignment as a power
        // of two, so anything larger makes the *linker* insert padding between
        // the sections it concatenates, and those NUL bytes land between the
        // newline-separated entries — corrupting every path after the first.
        // The damage appears only once objects are linked, so assert the
        // alignment here rather than the section contents.
        assert_eq!(section.align(), 1, "bitcode section must be byte-aligned");

        // And it round-trips through the reader.
        let extracted = extract_bitcode_filepaths_from_parsed_object(&after)
            .expect("Failed to extract embedded filepaths");
        assert_eq!(extracted, vec![PathBuf::from("/tmp/one.bc")]);
    }

    #[test]
    fn test_macho_build_version_survives_rebuild() {
        // Rebuilding an object drops anything the writer does not model. Losing
        // the platform load command makes the linker fall back to a guess and
        // warn on every object, so it has to be carried across explicitly.
        let data = create_minimal_macho_object();
        let in_object = object::File::parse(&*data).expect("Failed to parse the object");
        assert_eq!(in_object.format(), BinaryFormat::MachO);

        let expected = macho_build_version(&in_object);
        assert!(
            expected.is_some(),
            "the object carries no LC_BUILD_VERSION, so this test would prove nothing"
        );

        let rebuilt = copy_object_file(in_object).expect("Failed to rebuild the object");
        let rebuilt_data = rebuilt.write().expect("Failed to serialize the object");
        let rebuilt_object =
            object::File::parse(&*rebuilt_data).expect("Failed to parse the rebuilt object");

        assert_eq!(
            macho_build_version(&rebuilt_object),
            expected,
            "LC_BUILD_VERSION was not preserved across the rebuild"
        );
    }

    #[test]
    fn test_is_object_file_on_non_object() {
        // The argument parser classifies every unrecognized argument with this,
        // so a file that is not an object must answer "no" rather than raise.
        let dir = tempfile::tempdir().expect("Failed to create temp dir");

        let source_path = dir.path().join("hello.m");
        fs::write(&source_path, "int main(void) { return 0; }\n").expect("Failed to write");
        assert!(
            !is_object_file(&source_path).expect("a non-object must not be an error"),
            "a source file is not an object file"
        );

        // A real relocatable object still answers "yes".
        let object_path = dir.path().join("real.obj");
        create_minimal_coff_object(&object_path);
        assert!(is_object_file(&object_path).expect("Failed to classify"));
    }

    #[test]
    fn test_objcopy_section_specifier() {
        // Mach-O needs `segment,section`; the others name the section alone.
        assert_eq!(
            objcopy_section_specifier(BinaryFormat::MachO).unwrap(),
            format!("{DARWIN_SEGMENT_NAME},{DARWIN_SECTION_NAME}")
        );
        assert_eq!(
            objcopy_section_specifier(BinaryFormat::Elf).unwrap(),
            ELF_SECTION_NAME
        );
        assert_eq!(
            objcopy_section_specifier(BinaryFormat::Coff).unwrap(),
            COFF_SECTION_NAME
        );

        // WASM is handled by appending a custom section directly, never by
        // objcopy, so it must not produce a specifier.
        assert!(objcopy_section_specifier(BinaryFormat::Wasm).is_err());
    }

    #[test]
    fn test_path_injection_and_extraction() {
        let bitcode_pathbuf = tmp_bitcode("hello.bc");
        let bitcode_filepath = bitcode_pathbuf.as_path();

        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let object_pathbuf = dir.path().join("hello.o");
        fs::write(&object_pathbuf, create_minimal_macho_object()).expect("Failed to write");
        let object_filepath = object_pathbuf.as_path();
        let output_pathbuf = dir.path().join("hello.new.o");
        let output_object_filepath = output_pathbuf.as_path();

        // Embed bitcode filepath
        let ret = embed_bitcode_filepath_to_object_file(
            bitcode_filepath,
            object_filepath,
            Some(output_object_filepath),
        );
        assert!(ret.is_ok());

        // Extract embedded filepaths
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(output_object_filepath)
            .expect("Failed to extract embedded filepaths");
        assert!(!embedded_filepaths.is_empty());

        let expected_filepath = PathBuf::from(bitcode_filepath);
        println!("{:?}", embedded_filepaths[0]);
        assert_eq!(embedded_filepaths[0], expected_filepath);
    }

    #[test]
    fn test_paths_extraction() {
        // The linker concatenates the bitcode sections of the objects it
        // merges, so a linked artifact carries several newline-separated paths
        // in one section. Reproduce that shape directly.
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let object_filepath = dir.path().join("merged.obj");
        create_object_with_bitcode_paths(
            &object_filepath,
            &["/tmp/foo.bc", "/tmp/bar.bc", "/tmp/baz.bc", "/tmp/bar.bc"],
        );

        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&object_filepath)
            .expect("Failed to extract embedded filepaths");
        // Sorted and deduplicated: four entries, one repeated.
        assert_eq!(embedded_filepaths.len(), 3);

        let expected_filepaths = vec![
            PathBuf::from("/tmp/bar.bc"),
            PathBuf::from("/tmp/baz.bc"),
            PathBuf::from("/tmp/foo.bc"),
        ];
        println!("{:?}", embedded_filepaths);
        assert_eq!(embedded_filepaths, expected_filepaths)
    }

    /// Create a minimal COFF object file using the `object` crate's write API.
    /// Build a minimal Mach-O relocatable object carrying `LC_BUILD_VERSION`.
    ///
    /// Synthesized rather than read from a checked-in fixture so the crate
    /// ships no test binaries, and so the test runs on any host — a Mach-O
    /// object produced by the local compiler would be ELF on Linux.
    fn create_minimal_macho_object() -> Vec<u8> {
        use object::Architecture;

        let mut obj = write::Object::new(
            BinaryFormat::MachO,
            Architecture::Aarch64,
            object::Endianness::Little,
        );
        // `MachOBuildVersion` is non-exhaustive, so it has to be built by
        // assignment rather than a struct literal.
        let mut build_version = write::MachOBuildVersion::default();
        build_version.platform = object::macho::PLATFORM_MACOS;
        build_version.minos = object::macho::Version(0x000f_0000);
        build_version.sdk = object::macho::Version(0x000f_0000);
        obj.set_macho_build_version(build_version);
        let section_id = obj.add_section(b"__TEXT".to_vec(), b"__text".to_vec(), SectionKind::Text);
        // `ret` on arm64
        obj.section_mut(section_id)
            .set_data(&[0xc0, 0x03, 0x5f, 0xd6], 4);

        obj.write().expect("Failed to write Mach-O object")
    }

    /// Write a relocatable object whose bitcode section already holds several
    /// newline-separated paths, as it would after the linker concatenated the
    /// sections of several objects.
    fn create_object_with_bitcode_paths(path: &Path, paths: &[&str]) {
        use object::Architecture;

        let mut obj = write::Object::new(
            BinaryFormat::Coff,
            Architecture::X86_64,
            object::Endianness::Little,
        );
        let text_id = obj.add_section(vec![], b".text".to_vec(), SectionKind::Text);
        obj.section_mut(text_id).set_data(&[0xc3], 1);

        let payload: String = paths.iter().map(|p| format!("{p}\n")).collect();
        let section_id = obj.add_section(
            vec![],
            COFF_SECTION_NAME.as_bytes().to_vec(),
            SectionKind::Unknown,
        );
        let section = obj.section_mut(section_id);
        section.set_data(payload.as_bytes(), 1);
        section.flags = SectionFlags::Coff {
            characteristics: object::pe::SectionFlags(0),
        };

        let data = obj.write().expect("Failed to write object");
        fs::write(path, data).expect("Failed to write object file");
    }

    fn create_minimal_coff_object(path: &Path) {
        use object::Architecture;

        let mut obj = write::Object::new(
            BinaryFormat::Coff,
            Architecture::X86_64,
            object::Endianness::Little,
        );
        let section_id = obj.add_section(vec![], b".text".to_vec(), SectionKind::Text);
        let section = obj.section_mut(section_id);
        // A single `ret` instruction
        section.set_data(&[0xc3], 1);

        let data = obj.write().expect("Failed to write COFF object");
        fs::write(path, data).expect("Failed to write COFF file");
    }

    #[test]
    fn test_coff_path_injection_and_extraction() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let coff_obj_path = dir.path().join("test.obj");
        let output_path = dir.path().join("test.out.obj");

        create_minimal_coff_object(&coff_obj_path);

        let bitcode_pathbuf = tmp_bitcode("hello.bc");
        let bitcode_filepath = bitcode_pathbuf.as_path();

        // Embed bitcode filepath
        embed_bitcode_filepath_to_object_file(bitcode_filepath, &coff_obj_path, Some(&output_path))
            .expect("Failed to embed bitcode filepath into COFF object");

        // Extract embedded filepaths
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&output_path)
            .expect("Failed to extract embedded filepaths from COFF object");
        assert_eq!(embedded_filepaths.len(), 1);
        assert_eq!(embedded_filepaths[0], tmp_bitcode("hello.bc"));
    }

    #[test]
    fn test_coff_overwrite_in_place() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let coff_obj_path = dir.path().join("test.obj");

        create_minimal_coff_object(&coff_obj_path);

        let bitcode_pathbuf = tmp_bitcode("inplace.bc");
        let bitcode_filepath = bitcode_pathbuf.as_path();

        // Embed bitcode filepath in place (no output path)
        embed_bitcode_filepath_to_object_file::<&Path>(bitcode_filepath, &coff_obj_path, None)
            .expect("Failed to embed bitcode filepath into COFF object in place");

        // Extract
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&coff_obj_path)
            .expect("Failed to extract embedded filepaths from COFF object");
        assert_eq!(embedded_filepaths.len(), 1);
        assert_eq!(embedded_filepaths[0], tmp_bitcode("inplace.bc"));
    }

    #[test]
    fn test_coff_no_bitcode_section_returns_empty() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let coff_obj_path = dir.path().join("test.obj");

        create_minimal_coff_object(&coff_obj_path);

        // Extract from object with no bitcode section
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&coff_obj_path)
            .expect("Failed to extract from COFF object without bitcode section");
        assert!(embedded_filepaths.is_empty());
    }

    /// Create a minimal valid WASM binary file.
    ///
    /// Constructs a WASM module with the magic number, version header, and
    /// an empty type section. The `object` crate requires at least 16 bytes
    /// to detect the file format.
    fn create_minimal_wasm_object(path: &Path) {
        let mut data = vec![];
        // WASM magic number: \0asm
        data.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d]);
        // WASM version 1
        data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        // Type section (id=1), size=1, with 0 type entries
        data.extend_from_slice(&[0x01, 0x01, 0x00]);
        // Function section (id=3), size=1, with 0 function entries
        data.extend_from_slice(&[0x03, 0x01, 0x00]);
        // Code section (id=10), size=1, with 0 code entries
        data.extend_from_slice(&[0x0a, 0x01, 0x00]);

        fs::write(path, data).expect("Failed to write WASM file");
    }

    #[test]
    fn test_wasm_path_injection_and_extraction() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let wasm_obj_path = dir.path().join("test.wasm");
        let output_path = dir.path().join("test.out.wasm");

        create_minimal_wasm_object(&wasm_obj_path);

        let bitcode_pathbuf = tmp_bitcode("hello.bc");
        let bitcode_filepath = bitcode_pathbuf.as_path();

        // Embed bitcode filepath
        embed_bitcode_filepath_to_object_file(bitcode_filepath, &wasm_obj_path, Some(&output_path))
            .expect("Failed to embed bitcode filepath into WASM object");

        // Extract embedded filepaths
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&output_path)
            .expect("Failed to extract embedded filepaths from WASM object");
        assert_eq!(embedded_filepaths.len(), 1);
        assert_eq!(embedded_filepaths[0], tmp_bitcode("hello.bc"));
    }

    #[test]
    fn test_wasm_overwrite_in_place() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let wasm_obj_path = dir.path().join("test.wasm");

        create_minimal_wasm_object(&wasm_obj_path);

        let bitcode_pathbuf = tmp_bitcode("inplace.bc");
        let bitcode_filepath = bitcode_pathbuf.as_path();

        // Embed bitcode filepath in place (no output path)
        embed_bitcode_filepath_to_object_file::<&Path>(bitcode_filepath, &wasm_obj_path, None)
            .expect("Failed to embed bitcode filepath into WASM object in place");

        // Extract
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&wasm_obj_path)
            .expect("Failed to extract embedded filepaths from WASM object");
        assert_eq!(embedded_filepaths.len(), 1);
        assert_eq!(embedded_filepaths[0], tmp_bitcode("inplace.bc"));
    }

    #[test]
    fn test_wasm_no_bitcode_section_returns_empty() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let wasm_obj_path = dir.path().join("test.wasm");

        create_minimal_wasm_object(&wasm_obj_path);

        // Extract from object with no bitcode section
        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(&wasm_obj_path)
            .expect("Failed to extract from WASM object without bitcode section");
        assert!(embedded_filepaths.is_empty());
    }
}
