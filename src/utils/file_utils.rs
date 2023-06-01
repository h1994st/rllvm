//! File-related, especially object-file-related, utility functions

use std::{fs, path::Path};

use object::{Object, ObjectKind, ObjectSection};

use crate::error::Error;

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
) -> Result<(), Error>
where
    P: AsRef<Path>,
{
    let bitcode_filepath = bitcode_filepath.as_ref();
    let object_filepath = object_filepath.as_ref();

    let data = fs::read(object_filepath)?;
    let object_file = object::File::parse(&*data)?;

    if let Some(section) = object_file.section_by_name(".boot") {
        println!("{:#x?}", section.data()?);
    } else {
        eprintln!("section not available");
    }

    Ok(())
}
