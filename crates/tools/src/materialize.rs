//! Shared construction of a single translation unit's bitcode command.

use crate::{
    arg_parser::{universal_build_architectures, without_dependency_flags},
    error::Error,
};
use std::path::Path;

pub(crate) fn bitcode_arguments(
    compile_args: &[String],
    extra_args: &[String],
    source: &Path,
    output: &Path,
    depfile: Option<&Path>,
) -> Result<Vec<String>, Error> {
    let mut args = without_dependency_flags(compile_args);
    args.extend_from_slice(extra_args);
    let architectures = universal_build_architectures(&args);
    if architectures.len() > 1 {
        return Err(Error::InvalidArguments(format!(
            "universal builds are unsupported (architectures: {})",
            architectures.join(", ")
        )));
    }
    if let Some(depfile) = depfile {
        args.extend([
            "-MD".into(),
            "-MF".into(),
            depfile.to_string_lossy().into_owned(),
        ]);
    }
    args.extend([
        "-emit-llvm".into(),
        "-c".into(),
        "-o".into(),
        output.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn bitcode_command_preserves_flags_and_owns_dependency_output() {
        let flags = [
            "-O2",
            "-pthread",
            "-MMD",
            "-MForiginal.d",
            "-MT",
            "original.o",
        ]
        .map(String::from);
        let args = bitcode_arguments(
            &flags,
            &["-O0".into()],
            Path::new("source.c"),
            Path::new("module.bc"),
            Some(Path::new("private.d")),
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "-O2",
                "-pthread",
                "-O0",
                "-MD",
                "-MF",
                "private.d",
                "-emit-llvm",
                "-c",
                "-o",
                "module.bc",
                "source.c"
            ]
        );
    }

    #[test]
    fn bitcode_command_rejects_universal_builds() {
        let flags = ["-arch", "arm64", "-arch", "x86_64"].map(String::from);
        assert!(
            bitcode_arguments(
                &flags,
                &[],
                Path::new("source.c"),
                Path::new("module.bc"),
                None
            )
            .unwrap_err()
            .to_string()
            .contains("universal")
        );
    }
}
