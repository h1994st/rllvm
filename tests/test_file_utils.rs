#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rllvm::utils::*;

    #[test]
    fn test_path_injection() {
        let bitcode_filepath = Path::new("tests/data/hello.bc");
        let object_filepath = Path::new("tests/data/hello.o");

        let output_object_filepath = Path::new("/tmp/hello.new.o");

        let ret = embed_bitcode_filepath_to_object_file(
            bitcode_filepath,
            object_filepath,
            Some(output_object_filepath),
        );
        assert!(ret.is_ok());

        // Clean
        fs::remove_file(output_object_filepath).expect("Failed to delete the output object file");
    }

    #[test]
    fn test_path_extraction() {
        let bitcode_filepath = Path::new("tests/data/hello.bc");
        let object_filepath = Path::new("tests/data/hello.new.o");

        let embedded_filepaths = extract_bitcode_filepaths_from_object_file(object_filepath)
            .expect("Failed to extract embedded filepaths");
        assert!(!embedded_filepaths.is_empty());

        let embedded_filepath = embedded_filepaths[0].clone();
        let expected_filepath = bitcode_filepath
            .canonicalize()
            .expect("Failed to obtain the absolute filepath");
        println!("{:?}", embedded_filepath);
        assert_eq!(embedded_filepath, expected_filepath);
    }

    #[test]
    fn test_paths_extraction() {}
}
