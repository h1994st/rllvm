//! Filepath-related utility functions

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

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
    let parent_dir = src_filepath.parent().ok_or_else(|| {
        Error::InvalidArguments(format!(
            "Failed to obtain the parent directory: {:?}",
            src_filepath
        ))
    })?;
    // With extension
    let file_name = src_filepath
        .file_name()
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "Failed to obtain the file name: {:?}",
                src_filepath
            ))
        })?
        .to_str()
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "Failed to convert OsStr to str: {:?}",
                src_filepath
            ))
        })?;
    // Without extension
    let file_stem = src_filepath
        .file_stem()
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "Failed to obtain the file stem: {:?}",
                src_filepath
            ))
        })?
        .to_str()
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "Failed to convert OsStr to str: {:?}",
                src_filepath
            ))
        })?;

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

pub fn calculate_filepath_hash<P>(filepath: P) -> u64
where
    P: AsRef<Path>,
{
    let filepath = filepath.as_ref();

    let mut hasher = DefaultHasher::new();
    filepath.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn test_derive_object_and_bitcode_filepath() {
        let test_inputs = [
            (
                Path::new("/tmp/foo.c"),
                false,
                (Path::new("/tmp/.foo.o"), Path::new("/tmp/.foo.o.bc")),
            ),
            (
                Path::new("/tmp/foo.c"),
                true,
                (Path::new("/tmp/foo.c.o"), Path::new("/tmp/.foo.o.bc")),
            ),
        ];

        assert!(test_inputs.iter().all(
            |&(
                src_filepath,
                is_compile_only,
                (expected_object_filepath, expected_bitcode_filepath),
            )| {
                derive_object_and_bitcode_filepath(src_filepath, is_compile_only).is_ok_and(
                    |(object_filepath, bitcode_filepath)| {
                        object_filepath == expected_object_filepath
                            && bitcode_filepath == expected_bitcode_filepath
                    },
                )
            },
        ));
    }
}
