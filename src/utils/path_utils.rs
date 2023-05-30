use std::path::{Path, PathBuf};

use crate::error::Error;

pub fn derive_object_and_bitcode_filepath<P>(
    src_filepath: P,
    is_compile_only: bool,
) -> Result<(PathBuf, PathBuf), Error>
where
    P: AsRef<Path>,
{
    let src_filepath = src_filepath.as_ref();
    if !src_filepath.is_absolute() {
        return Err(Error::InvalidArguments(format!(
            "'src_filepath' must be absolute: {:?}",
            src_filepath
        )));
    }

    // Parent directory
    let parent_dir = src_filepath.parent().expect(&format!(
        "Failed to obtain the parent directory: {:?}",
        src_filepath
    ));
    // With extension
    let file_name = src_filepath
        .file_name()
        .expect(&format!(
            "Failed to obtain the file name: {:?}",
            src_filepath
        ))
        .to_str()
        .expect(&format!(
            "Failed to convert OsStr to str: {:?}",
            src_filepath
        ));
    // Without extension
    let file_stem = src_filepath
        .file_stem()
        .expect(&format!(
            "Failed to obtain the file stem: {:?}",
            src_filepath
        ))
        .to_str()
        .expect(&format!(
            "Failed to convert OsStr to str: {:?}",
            src_filepath
        ));

    let object_file_name = if is_compile_only {
        // Compile only. We need to explicitly generate the object file
        format!("{file_name}.o")
    } else {
        // Hide the object file, as it is only for bitcode generation
        format!(".{file_stem}.o")
    };
    // We always hide the bitcode file
    let bitcode_file_name = format!(".{file_stem}.o.bc");

    let object_filepath = parent_dir.join(object_file_name);
    let bitcode_filepath = parent_dir.join(bitcode_file_name);

    Ok((object_filepath, bitcode_filepath))
}
