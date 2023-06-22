use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::ExitStatus,
};

#[cfg(target_vendor = "apple")]
use glob::glob;
use which::which;

#[cfg(not(target_vendor = "apple"))]
use crate::constants::{LLVM_VERSION_MAX, LLVM_VERSION_MIN};
use crate::utils::{execute_command_for_status, execute_command_for_stdout_string};
use crate::{config::RLLVM_CONFIG, error::Error};

pub fn execute_llvm_ar<P, S>(llvm_ar_filepath: P, args: &[S]) -> Result<ExitStatus, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    execute_command_for_status(llvm_ar_filepath, args)
}

pub fn execute_llvm_link<P, S>(llvm_link_filepath: P, args: &[S]) -> Result<ExitStatus, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    execute_command_for_status(llvm_link_filepath, args)
}

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
    let llvm_config_glob_patterns = vec![
        // location for explicitly versioned brew formula
        format!("{brew_cellar_path}/llvm@*/{llvm_config_filepath_suffix}"),
        // location for current release brew formula
        format!("{brew_cellar_path}/llvm/{llvm_config_filepath_suffix}"),
    ];
    let glob_results = llvm_config_glob_patterns.iter().flat_map(|pattern| {
        glob(pattern).unwrap_or_else(|err| {
            panic!("Could not read glob pattern: pattern={pattern}, err={err}");
        })
    });
    match glob_results.last() {
        Some(llvm_config_filepath) => Ok(llvm_config_filepath.unwrap()),
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

        Err(Error::MissingFile(
            "Failed to find `llvm-config`".to_string(),
        ))
    }
}

/// Link given bitcode files into one bitcode file
///
/// TODO: do we need to link bitcode files incrementally in case the command
/// execeeds the limitation of `getconf ARG_MAX`?
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
    if let Some(llvm_link_flags) = RLLVM_CONFIG.llvm_link_flags() {
        args.extend(llvm_link_flags.iter().cloned());
    }
    // Output
    args.extend_from_slice(&[
        "-o".to_string(),
        String::from(output_filepath.to_string_lossy()),
    ]);
    // Input bitcode files
    args.extend(
        bitcode_filepaths
            .iter()
            .map(|x| String::from(x.as_ref().to_string_lossy())),
    );

    execute_command_for_status(RLLVM_CONFIG.llvm_link_filepath(), &args).map(|status| status.code())
}

/// Archive given bitcode files into one archive file
///
/// TODO:
/// 1. do we need to archive files incrementally?
/// 2. do we need to avoid absolute paths in the generated archive?
pub fn archive_bitcode_files<P>(
    bitcode_filepaths: &[P],
    output_filepath: P,
) -> Result<Option<i32>, Error>
where
    P: AsRef<Path>,
{
    let output_filepath = output_filepath.as_ref();

    let mut args = vec![
        "rs".to_string(),
        String::from(output_filepath.to_string_lossy()),
    ];
    // Input bitcode files
    args.extend(
        bitcode_filepaths
            .iter()
            .map(|x| String::from(x.as_ref().to_string_lossy())),
    );

    execute_command_for_status(RLLVM_CONFIG.llvm_ar_filepath(), &args).map(|status| status.code())
}
