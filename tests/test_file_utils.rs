#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use object::{Object, ObjectSection};
    use rllvm::utils::embed_bitcode_filepath_to_object_file;

    #[test]
    fn test_path_injection() {
        let bitcode_filepath = Path::new("tests/data/hello.bc");
        let object_filepath = Path::new("tests/data/hello.o");

        let output_object_filepath = Path::new("tests/data/hello.new.o");

        embed_bitcode_filepath_to_object_file(
            bitcode_filepath,
            object_filepath,
            Some(output_object_filepath),
        )
        .expect("Failed to embed bitcode filepath");

        let output_object_data =
            fs::read(output_object_filepath).expect("Failed to read the new object file");
        let output_object_file =
            object::File::parse(&*output_object_data).expect("Failed to parse the new object file");

        let section = output_object_file
            .section_by_name_bytes("__llvm_bc".as_bytes())
            .expect("Unable to obtain the section");

        let section_data = section.data().expect("Failed to obtain the section data");
        let embedded_filepath_string = String::from_utf8_lossy(section_data);

        let expected_filepath_string = format!(
            "{}\n",
            bitcode_filepath
                .canonicalize()
                .expect("Failed to obtain the absolute path")
                .to_string_lossy()
        );
        assert_eq!(embedded_filepath_string, expected_filepath_string);

        // Clean
        fs::remove_file(output_object_filepath).expect("Failed to delete the output object file");
    }
}
