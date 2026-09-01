//! Filepath-related utility functions

use std::{
    collections::hash_map::DefaultHasher,
    env,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use crate::error::Error;

/// Derive the object file and bitcode file paths from a source file path.
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

    // We always hide the bitcode file, alongside the source file
    let bitcode_filepath = parent_dir.join(format!(".{file_stem}.o.bc"));

    let object_filepath = if is_compile_only {
        // The compiler writes the object file itself. Absent an explicit `-o`,
        // `clang -c dir/foo.c` emits `foo.o` into the *current* directory, not
        // next to the source. Callers override this when `-o` is given.
        env::current_dir()?.join(format!("{file_stem}.o"))
    } else {
        // Hide the object file, as it exists only for bitcode generation
        parent_dir.join(format!(".{file_stem}.o"))
    };

    Ok((object_filepath, bitcode_filepath))
}

/// Compute a hash of the given file path for use in unique naming.
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
        let src_filepath = Path::new("/tmp/foo.c");

        // Linking: the object file is an internal artifact, hidden next to the
        // source file.
        let (object_filepath, bitcode_filepath) =
            derive_object_and_bitcode_filepath(src_filepath, false)
                .expect("Failed to derive filepaths");
        assert_eq!(object_filepath, Path::new("/tmp/.foo.o"));
        assert_eq!(bitcode_filepath, Path::new("/tmp/.foo.o.bc"));

        // Compile-only: the compiler writes `foo.o` into the current working
        // directory, which is where the bitcode path must be embedded.
        let (object_filepath, bitcode_filepath) =
            derive_object_and_bitcode_filepath(src_filepath, true)
                .expect("Failed to derive filepaths");
        assert_eq!(
            object_filepath,
            env::current_dir().unwrap().join("foo.o"),
            "compile-only object file belongs in the working directory"
        );
        assert_eq!(bitcode_filepath, Path::new("/tmp/.foo.o.bc"));
    }
}
