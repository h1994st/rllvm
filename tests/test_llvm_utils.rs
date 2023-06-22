#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use rllvm::{
        compiler_wrapper::{llvm::ClangWrapper, CompilerKind, CompilerWrapper},
        utils::*,
    };

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

    fn build_bitcode_files(label: &str) -> bool {
        let bitcode_filepaths = [
            PathBuf::from(format!("/tmp/{}_bar.bc", label)),
            PathBuf::from(format!("/tmp/{}_baz.bc", label)),
            PathBuf::from(format!("/tmp/{}_foo.bc", label)),
        ];

        let input_args = [
            [
                "-c",
                "-emit-llvm",
                "-o",
                bitcode_filepaths[0].to_str().unwrap(),
                "tests/data/bar.c",
            ],
            [
                "-c",
                "-emit-llvm",
                "-o",
                bitcode_filepaths[1].to_str().unwrap(),
                "tests/data/baz.c",
            ],
            [
                "-c",
                "-emit-llvm",
                "-o",
                bitcode_filepaths[2].to_str().unwrap(),
                "tests/data/foo.c",
            ],
        ];

        input_args.iter().all(|args| {
            let mut cc = ClangWrapper::new("rllvm", CompilerKind::Clang);
            cc.parse_args(args)
                .unwrap()
                .run()
                .unwrap()
                .map_or(false, |code| code == 0)
        })
    }

    #[test]
    fn test_link_bitcode_files() {
        // Prepare input bitcode files
        assert!(build_bitcode_files("link"));

        let bitcode_filepaths = [
            Path::new("/tmp/link_bar.bc"),
            Path::new("/tmp/link_baz.bc"),
            Path::new("/tmp/link_foo.bc"),
        ];

        let output_filepath = Path::new("/tmp/foo_bar_baz.bc");

        assert!(
            link_bitcode_files(&bitcode_filepaths, output_filepath).map_or_else(
                |err| {
                    println!("Failed to link bitcode files: {:?}", err);
                    false
                },
                |code| { code.map_or(false, |code| code == 0) }
            )
        );

        // Check if the output file is successfully created
        assert!(output_filepath.exists() && output_filepath.is_file());

        // Clean
        fs::remove_file(output_filepath).expect("Failed to delete the output bitcode file");
        bitcode_filepaths.iter().for_each(|&bitcode_filepath| {
            fs::remove_file(bitcode_filepath).expect("Failed to delete the input bitcode file")
        });
    }

    #[test]
    fn test_archive_bitcode_files() {
        // Prepare input bitcode files
        assert!(build_bitcode_files("archive"));

        let bitcode_filepaths = [
            Path::new("/tmp/archive_bar.bc"),
            Path::new("/tmp/archive_baz.bc"),
            Path::new("/tmp/archive_foo.bc"),
        ];

        let output_filepath = Path::new("/tmp/foo_bar_baz.bca");

        assert!(
            archive_bitcode_files(&bitcode_filepaths, output_filepath).map_or_else(
                |err| {
                    println!("Failed to archive bitcode files: {:?}", err);
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
                    println!("Failed to parse the output file: {:?}", err);
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
        bitcode_filepaths.iter().for_each(|&bitcode_filepath| {
            fs::remove_file(bitcode_filepath).expect("Failed to delete the input bitcode file")
        });
    }
}
