use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    str,
};

#[cfg(target_vendor = "apple")]
use glob::glob;
use which::which;

#[cfg(not(target_vendor = "apple"))]
use crate::constants::{LLVM_VERSION_MAX, LLVM_VERSION_MIN};
use crate::error::Error;

pub fn execute_llvm_config<P, S>(llvm_config_filepath: P, args: &[S]) -> Result<String, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    let llvm_config_filepath = llvm_config_filepath.as_ref();
    let output = Command::new(llvm_config_filepath).args(args).output()?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Heuristically searching for `llvm-config` in Homebrew (for macOS)
///
/// NOTE: this function is borrowed from `AFLplusplus/LibAFL`
#[cfg(target_vendor = "apple")]
fn find_llvm_config_brew() -> Result<PathBuf, Error> {
    let output = Command::new("brew").arg("--cellar").output()?;
    let brew_cellar_path = str::from_utf8(&output.stdout).unwrap_or_default().trim();
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

        Err(Error::Unknown("Failed to find `llvm-config`".to_string()))
    }
}
