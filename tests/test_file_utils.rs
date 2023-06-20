#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use rllvm::utils::*;

    #[test]
    fn test_path_injection_and_extraction() {
        let bitcode_filepath = Path::new("tests/data/hello.bc");
        let object_filepath = Path::new("tests/data/hello.o");

        let output_object_filepath = Path::new("/tmp/hello.new.o");

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

        let embedded_filepath = embedded_filepaths[0].clone();
        let expected_filepath = bitcode_filepath
            .canonicalize()
            .expect("Failed to obtain the absolute filepath");
        println!("{:?}", embedded_filepath);
        assert_eq!(embedded_filepath, expected_filepath);

        // Clean
        fs::remove_file(output_object_filepath).expect("Failed to delete the output object file");
    }

    #[test]
    fn test_paths_extraction() {
        let object_filepath = Path::new("tests/data/foo_bar_baz.dylib");

        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(object_filepath)
            .expect("Failed to extract embedded filepaths");
        assert_eq!(embedded_filepaths.len(), 3);

        let expected_filepaths = vec![
            PathBuf::from("/tmp/bar.bc"),
            PathBuf::from("/tmp/baz.bc"),
            PathBuf::from("/tmp/foo.bc"),
        ];
        println!("{:?}", embedded_filepaths);
        assert_eq!(embedded_filepaths, expected_filepaths)
    }
}
