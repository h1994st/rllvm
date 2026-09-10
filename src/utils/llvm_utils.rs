use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[cfg(target_vendor = "apple")]
use glob::glob;
use which::which;

#[cfg(not(target_vendor = "apple"))]
use crate::constants::{LLVM_VERSION_MAX, LLVM_VERSION_MIN};
use crate::utils::{execute_command_for_stdout_string, execute_llvm_tool};
use crate::{config::try_rllvm_config, error::Error};

/// Execute `llvm-config` with the given arguments and return stdout.
pub fn execute_llvm_config<P, S>(llvm_config_filepath: P, args: &[S]) -> Result<String, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    execute_command_for_stdout_string(llvm_config_filepath, args)
}

/// Heuristically searching for `llvm-config` in Homebrew (for macOS)
///
/// NOTE: this function is borrowed from `AFLplusplus/LibAFL`
#[cfg(target_vendor = "apple")]
fn find_llvm_config_brew() -> Result<PathBuf, Error> {
    let brew_cellar_path = execute_command_for_stdout_string("brew", &["--cellar"])?;
    if brew_cellar_path.is_empty() {
        return Err(Error::ExecutionFailure(
            "Empty return from `brew --cellar`".to_string(),
        ));
    }
    let llvm_config_filepath_suffix = "*/bin/llvm-config";
    let llvm_config_glob_patterns = [
        // location for explicitly versioned brew formula
        format!("{brew_cellar_path}/llvm@*/{llvm_config_filepath_suffix}"),
        // location for current release brew formula
        format!("{brew_cellar_path}/llvm/{llvm_config_filepath_suffix}"),
    ];
    let mut candidates = vec![];
    for pattern in &llvm_config_glob_patterns {
        let matches = glob(pattern).map_err(|err| {
            Error::InvalidArguments(format!(
                "Could not read glob pattern: pattern={pattern}, err={err}"
            ))
        })?;
        // Entries that cannot be read (permissions, races) are skipped rather
        // than aborting the search.
        candidates.extend(matches.flatten());
    }
    match candidates.last() {
        Some(llvm_config_filepath) => Ok(llvm_config_filepath.clone()),
        None => Err(Error::Unknown(format!(
            "Failed to find `llvm-config` in brew cellar with glob patterns: {}",
            llvm_config_glob_patterns.join(" ")
        ))),
    }
}

/// Heuristically searching for the filepath of `llvm-config`
///
/// NOTE: this function is borrowed from `AFLplusplus/LibAFL`
pub fn find_llvm_config() -> Result<PathBuf, Error> {
    if let Ok(var) = env::var("LLVM_CONFIG") {
        return Ok(PathBuf::from(var).canonicalize()?);
    }

    if let Ok(llvm_config_filepath) = which("llvm-config") {
        return Ok(llvm_config_filepath);
    }

    #[cfg(target_vendor = "apple")]
    {
        find_llvm_config_brew()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        for version in (LLVM_VERSION_MIN..=LLVM_VERSION_MAX).rev() {
            let llvm_config_name: String = format!("llvm-config-{version}");
            if let Ok(llvm_config_filepath) = which(&llvm_config_name) {
                return Ok(llvm_config_filepath);
            }
        }

        Err(Error::MissingFile(format!(
            "Failed to find `llvm-config` (searched PATH and versioned names llvm-config-{{{LLVM_VERSION_MIN}..{LLVM_VERSION_MAX}}})"
        )))
    }
}

/// Link given bitcode files into one bitcode file
pub fn link_bitcode_files<P>(
    bitcode_filepaths: &[P],
    output_filepath: P,
) -> Result<Option<i32>, Error>
where
    P: AsRef<Path>,
{
    let output_filepath = output_filepath.as_ref();

    let mut args = vec![];
    // Link arguments
    if let Some(llvm_link_flags) = try_rllvm_config()?.llvm_link_flags() {
        args.extend(llvm_link_flags.iter().cloned());
    }
    // Output
    args.extend_from_slice(&[
        "-o".to_string(),
        output_filepath.to_string_lossy().into_owned(),
    ]);
    // Input bitcode files
    args.extend(
        bitcode_filepaths
            .iter()
            .map(|x| x.as_ref().to_string_lossy().into_owned()),
    );

    execute_llvm_tool(try_rllvm_config()?.llvm_link_filepath(), &args).map(|status| status.code())
}

/// Archive given bitcode files into one archive file
///
/// TODO: do we need to avoid absolute paths in the generated archive?
pub fn archive_bitcode_files<P>(
    bitcode_filepaths: &[P],
    output_filepath: P,
) -> Result<Option<i32>, Error>
where
    P: AsRef<Path>,
{
    let output_filepath = std::path::absolute(output_filepath.as_ref())?;
    let output_dir = output_filepath.parent().ok_or_else(|| {
        Error::InvalidArguments(format!("Archive output has no parent: {output_filepath:?}"))
    })?;
    // `llvm-ar r` updates an existing archive without removing old members.
    // Build a fresh archive beside the destination, then replace it only
    // after success. The same filesystem makes the rename atomic.
    let workspace = tempfile::Builder::new()
        .prefix(".rllvm-archive-")
        .tempdir_in(output_dir)?;
    let staged_archive = workspace.path().join("archive.bca");

    let mut args = vec![
        "rs".to_string(),
        staged_archive.to_string_lossy().into_owned(),
    ];
    // Input bitcode files
    args.extend(
        bitcode_filepaths
            .iter()
            .map(|x| x.as_ref().to_string_lossy().into_owned()),
    );

    let status = execute_llvm_tool(try_rllvm_config()?.llvm_ar_filepath(), &args)?;
    if status.success() {
        std::fs::rename(&staged_archive, &output_filepath)?;
    }
    Ok(status.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler_wrapper::{CompilerKind, CompilerWrapper, llvm::ClangWrapper},
        constants::RESPONSE_FILE_ARGUMENT_THRESHOLD,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    #[test]
    fn finds_llvm_config() {
        assert!(find_llvm_config().is_ok_and(|llvm_config_path| {
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

    /// Compile three small sources to bitcode inside a temporary directory.
    ///
    /// The sources are written here rather than read from `tests/data`, so the
    /// published crate needs no fixture files, and the temporary directory
    /// keeps concurrent test runs from colliding in `/tmp`.
    fn build_bitcode_files(dir: &Path) -> Vec<PathBuf> {
        let sources = [
            ("bar", "int bar(int a) { return a + 1; }\n"),
            (
                "baz",
                "float baz_max(double a, double b) { return (float)(a > b ? a : b); }\n",
            ),
            ("foo", "int foo(int a, int b) { return a + b; }\n"),
        ];

        sources
            .iter()
            .map(|(name, contents)| build_bitcode_file(dir, name, contents))
            .collect()
    }

    /// Compile one source into a bitcode file beside it.
    fn build_bitcode_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let source_path = dir.join(format!("{name}.c"));
        fs::write(&source_path, contents).expect("Failed to write the source file");

        let bitcode_path = dir.join(format!("{name}.bc"));
        let args = [
            "-c",
            "-emit-llvm",
            "-o",
            bitcode_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ];

        let mut cc = ClangWrapper::new("rllvm", CompilerKind::Clang)
            .expect("Failed to build the clang wrapper");
        assert_eq!(
            cc.parse_args(&args).unwrap().run().unwrap(),
            Some(0),
            "Failed to generate bitcode for {name}.c"
        );

        bitcode_path
    }

    /// Compile one empty module at a deliberately long path.
    ///
    /// `ARG_MAX` counts bytes, not arguments, so overflowing it takes paths
    /// that are long as well as numerous: a few thousand short temp paths fit
    /// comfortably. Four 200-byte components stay under the 255-byte component
    /// limit and leave room under macOS's 1024-byte `PATH_MAX`.
    ///
    /// The module is empty so it defines no symbols, which is what lets the
    /// same file be linked against itself thousands of times without
    /// colliding. One compile keeps the test fast.
    fn build_deeply_nested_empty_module(dir: &Path) -> PathBuf {
        let mut nested = dir.to_path_buf();
        for _ in 0..4 {
            nested = nested.join("d".repeat(200));
        }
        fs::create_dir_all(&nested).expect("Failed to create the nested directory");

        build_bitcode_file(&nested, "empty", "")
    }

    /// Number of copies that puts the command line past `ARG_MAX` on every
    /// supported host: roughly 3.8 MB at the path length above, against a
    /// 1 MB limit on macOS and 2 MB on Linux.
    const OVERSIZED_ARGUMENT_COUNT: usize = 4096;

    #[test]
    fn links_more_bitcode_files_than_fit_in_an_argument_list() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let module = build_deeply_nested_empty_module(dir.path());
        let bitcode_filepaths = vec![module; OVERSIZED_ARGUMENT_COUNT];
        let output_pathbuf = dir.path().join("linked.bc");

        assert_eq!(
            link_bitcode_files(&bitcode_filepaths, output_pathbuf.clone()).unwrap(),
            Some(0)
        );
        assert!(output_pathbuf.is_file());
    }

    #[test]
    fn archives_more_bitcode_files_than_fit_in_an_argument_list() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let module = build_deeply_nested_empty_module(dir.path());
        let bitcode_filepaths = vec![module; OVERSIZED_ARGUMENT_COUNT];
        let output_pathbuf = dir.path().join("archived.bca");

        assert_eq!(
            archive_bitcode_files(&bitcode_filepaths, output_pathbuf.clone()).unwrap(),
            Some(0)
        );
        assert!(output_pathbuf.is_file());
    }

    /// The response file is text, so a path containing whitespace or a quote
    /// splits into two arguments unless it is escaped.
    ///
    /// This asserts on what `llvm-link` produced, not on the escaped string:
    /// an assertion against `escape_response_file_argument`'s own output would
    /// hold just as well if that output were wrong for LLVM's tokenizer.
    #[test]
    fn links_bitcode_files_whose_paths_need_escaping() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let awkward = dir.path().join(r#"a dir with 'quotes' and "spaces""#);
        fs::create_dir_all(&awkward).expect("Failed to create the awkward directory");

        // Enough copies to cross the threshold, so the paths travel through the
        // response file rather than the argument list. Empty, so the repetition
        // defines no symbol twice.
        let module = build_bitcode_file(&awkward, "escaped", "");
        let bitcode_filepaths = vec![module; RESPONSE_FILE_ARGUMENT_THRESHOLD + 1];
        let output_pathbuf = dir.path().join("escaped_out.bc");

        assert_eq!(
            link_bitcode_files(&bitcode_filepaths, output_pathbuf.clone()).unwrap(),
            Some(0)
        );
        assert!(output_pathbuf.is_file());
    }

    #[test]
    fn links_bitcode_files() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let bitcode_filepaths = build_bitcode_files(dir.path());
        let output_pathbuf = dir.path().join("foo_bar_baz.bc");
        let output_filepath = output_pathbuf.as_path();

        assert!(
            link_bitcode_files(&bitcode_filepaths, output_pathbuf.clone()).map_or_else(
                |err| {
                    println!("Failed to link bitcode files: {:?}", err);
                    false
                },
                |code| { code == Some(0) }
            )
        );

        // Check if the output file is successfully created
        assert!(output_filepath.exists() && output_filepath.is_file());
    }

    #[test]
    fn archives_bitcode_files() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let bitcode_filepaths = build_bitcode_files(dir.path());
        let output_pathbuf = dir.path().join("foo_bar_baz.bca");
        let output_filepath = output_pathbuf.as_path();

        assert!(
            archive_bitcode_files(&bitcode_filepaths, output_pathbuf.clone()).map_or_else(
                |err| {
                    println!("Failed to archive bitcode files: {:?}", err);
                    false
                },
                |code| { code == Some(0) }
            )
        );

        // Check if the output file is successfully created
        assert!(output_filepath.exists() && output_filepath.is_file());

        // Check the type of the output archive
        let output_data = fs::read(output_filepath).expect("Failed to read the output file");
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

        // The TempDir cleans up every artifact when it drops.
    }
}
