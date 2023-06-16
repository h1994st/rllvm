//! File-related, especially object-file-related, utility functions

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    str,
};

use object::{
    write, BinaryFormat, File, Object, ObjectComdat, ObjectKind, ObjectSection, ObjectSymbol,
    RelocationTarget, SectionFlags, SectionKind, SymbolFlags, SymbolKind, SymbolSection,
};

use crate::{
    constants::{DARWIN_SECTION_NAME, DARWIN_SEGMENT_NAME, ELF_SECTION_NAME},
    error::Error,
};

pub fn is_plain_file<P>(file: P) -> bool
where
    P: AsRef<Path>,
{
    let file = file.as_ref();
    if !file.exists() {
        false
    } else if file.is_dir() {
        false
    } else {
        true
    }
}

pub fn is_object_file<P>(file: P) -> Result<bool, Error>
where
    P: AsRef<Path>,
{
    let file = file.as_ref();

    if !is_plain_file(file) {
        return Ok(false);
    }

    let data = fs::read(file)?;
    let object_file = object::File::parse(&*data)?;

    Ok(object_file.kind() == ObjectKind::Relocatable)
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

    // Platform-dependent properties
    let (segment_name, section_name, flags) = match object_binary_format {
        BinaryFormat::Elf => (
            vec![],
            ELF_SECTION_NAME.as_bytes().to_vec(),
            SectionFlags::Elf { sh_flags: 0 },
        ),
        BinaryFormat::MachO => (
            DARWIN_SEGMENT_NAME.as_bytes().to_vec(),
            DARWIN_SECTION_NAME.as_bytes().to_vec(),
            SectionFlags::MachO { flags: 0 },
        ),
        _ => unimplemented!(),
    };

    // Copy the input object file into a new mutable object file
    let mut new_object_file = copy_object_file(object_file)?;

    // Add a section
    let section_id = new_object_file.add_section(segment_name, section_name, SectionKind::Unknown);
    let new_section = new_object_file.section_mut(section_id);
    let bitcode_filepath_string =
        format!("{}\n", bitcode_filepath.canonicalize()?.to_string_lossy());
    new_section.set_data(bitcode_filepath_string.as_bytes(), 1);
    // NOTE: we have to explicitly set flags; otherwise, the flags will be
    // inferred based on the section kind, but `Section::Unknown` is not
    // supported for auto inferring flags
    new_section.flags = flags;

    let output_data = new_object_file.write().unwrap();
    if let Some(output_object_filepath) = output_object_filepath {
        // Save the new object file
        fs::write(output_object_filepath, output_data)?;
    } else {
        // Overwrite the input object file
        fs::write(object_filepath, output_data)?;
    }

    Ok(())
}

fn copy_object_file(in_object: File) -> Result<write::Object, Error> {
    if in_object.kind() != ObjectKind::Relocatable {
        return Err(Error::InvalidArguments(format!(
            "Unsupported object kink: {:?}",
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
        if in_symbol.kind() == SymbolKind::Null {
            continue;
        }

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
                )))
            }
        };
        let flags = match in_symbol.flags() {
            SymbolFlags::None => SymbolFlags::None,
            SymbolFlags::Elf { st_info, st_other } => SymbolFlags::Elf { st_info, st_other },
            SymbolFlags::MachO { n_desc } => SymbolFlags::MachO { n_desc },
            SymbolFlags::CoffSection {
                selection,
                associative_section,
            } => {
                let associative_section =
                    associative_section.map(|index| *out_sections.get(&index).unwrap());
                SymbolFlags::CoffSection {
                    selection,
                    associative_section,
                }
            }
            SymbolFlags::Xcoff {
                n_sclass,
                x_smtyp,
                x_smclas,
                containing_csect,
            } => {
                let containing_csect =
                    containing_csect.map(|index| *out_symbols.get(&index).unwrap());
                SymbolFlags::Xcoff {
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
                )))
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
                    )))
                }
            };
            let out_relocation = write::Relocation {
                offset,
                size: in_relocation.size(),
                kind: in_relocation.kind(),
                encoding: in_relocation.encoding(),
                symbol,
                addend: in_relocation.addend(),
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

/// Extract the path of the bitcode from the corresponding object file
pub fn extract_bitcode_filepath_from_object_file<P>(
    object_filepath: P,
) -> Result<Option<Vec<PathBuf>>, Error>
where
    P: AsRef<Path>,
{
    let object_filepath = object_filepath.as_ref();

    let data = fs::read(object_filepath)?;
    let object_file = object::File::parse(&*data)?;
    let object_binary_format = object_file.format();

    let section_name = match object_binary_format {
        BinaryFormat::Elf => ELF_SECTION_NAME.as_bytes(),
        BinaryFormat::MachO => DARWIN_SECTION_NAME.as_bytes(),
        _ => unimplemented!(),
    };

    match object_file.section_by_name_bytes(section_name) {
        Some(section) => {
            let section_data = section.data()?;
            let embedded_filepath_string = str::from_utf8(section_data)?.trim();

            let embedded_filepaths = embedded_filepath_string
                .split('\n')
                .map(|x| PathBuf::from(x))
                .collect();

            Ok(Some(embedded_filepaths))
        }
        None => Ok(None),
    }
}
