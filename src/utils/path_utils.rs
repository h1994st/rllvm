//! Filepath-related utility functions

use std::{
    env,
    path::{Path, PathBuf},
};

use crate::error::Error;

/// Derive the object file and bitcode file paths from a source file path.
pub(crate) fn derive_object_and_bitcode_filepath<P>(
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

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Compute a stable hash of the given file path for use in unique naming.
///
/// This is FNV-1a, spelled out here rather than taken from a crate or from
/// `DefaultHasher`. `DefaultHasher` is explicitly *not* guaranteed to stay the
/// same across Rust releases, and bitcode files in `bitcode_store_path` are
/// named after this value: a toolchain upgrade would rename every future
/// artifact and orphan everything already in the store.
///
/// The bytes hashed come from [`Path::to_string_lossy`], not from the
/// platform's native `OsStr` encoding (raw bytes on Unix, UTF-16 on Windows),
/// so the same textual path yields the same hash on every platform and
/// architecture. Non-Unicode paths hash through the replacement character and
/// can therefore collide; the store filename also carries the source file stem,
/// so a collision needs two non-Unicode paths that share a stem.
pub fn calculate_filepath_hash<P>(filepath: P) -> u64
where
    P: AsRef<Path>,
{
    let filepath = filepath.as_ref();

    let mut hash = FNV_OFFSET_BASIS;
    for byte in filepath.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_derive_object_and_bitcode_filepath() {
        // Must be absolute on the host platform: `/tmp/foo.c` is not absolute
        // on Windows, where paths need a drive prefix.
        let src_dir = env::temp_dir();
        let src_filepath = src_dir.join("foo.c");
        let src_filepath = src_filepath.as_path();

        // Linking: the object file is an internal artifact, hidden next to the
        // source file.
        let (object_filepath, bitcode_filepath) =
            derive_object_and_bitcode_filepath(src_filepath, false)
                .expect("Failed to derive filepaths");
        assert_eq!(object_filepath, src_dir.join(".foo.o"));
        assert_eq!(bitcode_filepath, src_dir.join(".foo.o.bc"));

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
        assert_eq!(bitcode_filepath, src_dir.join(".foo.o.bc"));
    }

    #[test]
    fn test_calculate_filepath_hash_is_stable() {
        // Hard-coded expectations, deliberately: this hash names files in the
        // bitcode store, so it must survive Rust upgrades and be identical on
        // every platform. If these values change, the store is orphaned.
        assert_eq!(
            calculate_filepath_hash(Path::new("")),
            0xcbf2_9ce4_8422_2325
        );
        assert_eq!(
            calculate_filepath_hash(Path::new("/tmp/foo.c")),
            6720249941370504407
        );
        assert_eq!(
            calculate_filepath_hash(Path::new(
                "/home/user/projects/very/deeply/nested/directory/structure/with/many/components/source_file.c"
            )),
            16351161328938945821
        );
    }

    #[test]
    fn test_calculate_filepath_hash_distinguishes_paths() {
        let foo = calculate_filepath_hash(Path::new("/tmp/foo.c"));
        let bar = calculate_filepath_hash(Path::new("/tmp/bar.c"));
        let nested = calculate_filepath_hash(Path::new("/tmp/sub/foo.c"));

        assert_ne!(foo, bar, "different file stems must hash differently");
        assert_ne!(foo, nested, "different directories must hash differently");

        // Same input, same hash.
        assert_eq!(foo, calculate_filepath_hash(PathBuf::from("/tmp/foo.c")));
    }
}
