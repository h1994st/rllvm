#[cfg(test)]
mod tests {
    use std::path::Path;

    use rllvm::utils::*;

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
                derive_object_and_bitcode_filepath(src_filepath, is_compile_only).map_or(
                    false,
                    |(object_filepath, bitcode_filepath)| {
                        object_filepath == expected_object_filepath
                            && bitcode_filepath == expected_bitcode_filepath
                    },
                )
            },
        ));
    }
}
