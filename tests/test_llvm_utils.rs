#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rllvm::utils::*;

    #[test]
    fn test_find_llvm_config() {
        assert!(find_llvm_config().map_or(false, |llvm_config_path| {
            println!("llvm_config_path={:?}", llvm_config_path);
            llvm_config_path.exists()
                && llvm_config_path.is_file()
                && llvm_config_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("llvm-config")
        }));
    }

    #[test]
    fn test_link_bitcode_files() {
        let bitcode_filepaths = [
            Path::new("tests/data/bar.bc"),
            Path::new("tests/data/baz.bc"),
            Path::new("tests/data/foo.bc"),
        ];

        let output_filepath = Path::new("/tmp/foo_bar_baz.bc");

        assert!(
            link_bitcode_files(&bitcode_filepaths, output_filepath).map_or_else(
                |err| {
                    eprintln!("Failed to link bitcode files: {:?}", err);
                    false
                },
                |code| { code.map_or(false, |code| code == 0) }
            )
        );

        // Check if the output file is successfully created
        assert!(output_filepath.exists() && output_filepath.is_file());

        // Clean
        fs::remove_file(output_filepath).expect("Failed to delete the output bitcode file");
    }

    #[test]
    fn test_archive_bitcode_files() {
        let bitcode_filepaths = [
            Path::new("tests/data/bar.bc"),
            Path::new("tests/data/baz.bc"),
            Path::new("tests/data/foo.bc"),
        ];

        let output_filepath = Path::new("foo_bar_baz.bca");

        assert!(
            archive_bitcode_files(&bitcode_filepaths, output_filepath).map_or_else(
                |err| {
                    eprintln!("Failed to link bitcode files: {:?}", err);
                    false
                },
                |code| { code.map_or(false, |code| code == 0) }
            )
        );

        // Check if the output file is successfully created
        assert!(output_filepath.exists() && output_filepath.is_file());

        // Check the type of the output archive
        let output_data = fs::read(&output_filepath).expect("Failed to read the output file");
        assert!(
            object::read::archive::ArchiveFile::parse(&*output_data).map_or_else(
                |err| {
                    eprintln!("Failed to archive bitcode files: {:?}", err);
                    false
                },
                |output_archive_file| {
                    println!("Output archive file kind: {:?}", output_archive_file.kind());
                    true
                },
            )
        );

        // Clean
        fs::remove_file(output_filepath).expect("Failed to delete the output bitcode file");
    }
}
