#[cfg(test)]
mod tests {
    use std::{fs, path::Path, str};

    use object::{Object, ObjectSection};
    use rllvm::utils::embed_bitcode_filepath_to_object_file;

    #[test]
    fn test_path_injection() {
        let bitcode_filepath = Path::new("hello.bc");
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
        let embedded_path =
            Path::new(str::from_utf8(section_data).expect("Failed to convert data to string"));
        assert_eq!(embedded_path, bitcode_filepath);

        // Clean
        fs::remove_file(output_object_filepath).expect("Failed to delete the output object file");
    }
}
