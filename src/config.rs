//! TOML-based configuration for rllvm.
//!
//! Configuration is loaded from `~/.rllvm/config.toml` by default, or from a path
//! specified via the `RLLVM_CONFIG` environment variable. The configuration stores
//! paths to LLVM tools (`clang`, `llvm-link`, etc.) and optional flags for bitcode
//! generation and linking.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use tracing::Level;

use crate::{
    constants::{
        DEFAULT_CONF_FILEPATH_UNDER_HOME, DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME, HOME_ENV_NAME,
    },
    diagnostics::{check_version_compatibility, print_missing_tool_error},
    error::Error,
    utils::{execute_llvm_config, find_llvm_config},
};

/// The cached outcome of loading the configuration.
///
/// The failure is stored as a message rather than as an [`Error`], because a
/// `OnceLock` hands out shared references and the error type is not `Clone` — every
/// caller needs its own owned error.
type ConfigResult = Result<RLLVMConfig, String>;

fn config_result_to_ref(result: &'static ConfigResult) -> Result<&'static RLLVMConfig, Error> {
    match result {
        Ok(config) => Ok(config),
        Err(message) => Err(Error::ConfigError(message.clone())),
    }
}

#[cfg(not(test))]
pub fn try_rllvm_config() -> Result<&'static RLLVMConfig, Error> {
    static RLLVM_CONFIG: OnceLock<ConfigResult> = OnceLock::new();
    config_result_to_ref(RLLVM_CONFIG.get_or_init(|| {
        RLLVMConfig::new().map_err(|err| format!("Failed to load rllvm configuration: {err}"))
    }))
}

/// Returns the global [`RLLVMConfig`] singleton (test variant).
///
/// Uses [`RLLVMConfig::try_default`] to infer configuration from the system.
#[cfg(test)]
pub fn try_rllvm_config() -> Result<&'static RLLVMConfig, Error> {
    static RLLVM_CONFIG: OnceLock<ConfigResult> = OnceLock::new();
    config_result_to_ref(RLLVM_CONFIG.get_or_init(|| {
        RLLVMConfig::try_default()
            .map_err(|err| format!("Failed to infer rllvm configuration: {err}"))
    }))
}

/// Returns the path the configuration is read from, and written to.
///
/// `$RLLVM_CONFIG` when set, otherwise `~/.rllvm/config.toml`.
///
/// Every component that needs to know where the configuration lives must go
/// through this. The path used to be decided in two places — here for reading
/// and in `rllvm-init` for writing — and they disagreed: `rllvm-init` hardcoded
/// the home path and ignored `RLLVM_CONFIG` entirely, so it could report writing
/// a configuration that nothing would ever read, while silently overwriting the
/// user's real one.
pub fn config_filepath() -> PathBuf {
    env::var(DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME).map_or_else(
        |_| {
            // Default config file
            PathBuf::from(env::var(HOME_ENV_NAME).unwrap_or("".into()))
                .join(DEFAULT_CONF_FILEPATH_UNDER_HOME)
        },
        // User-defined config file
        PathBuf::from,
    )
}

/// Configuration for rllvm, specifying LLVM tool paths and optional flags.
///
/// Typically loaded from `~/.rllvm/config.toml` via [`try_rllvm_config`], or
/// inferred from the system using [`RLLVMConfig::try_default`].
#[derive(Serialize, Deserialize, Debug)]
pub struct RLLVMConfig {
    /// The absolute filepath of `llvm-config`
    llvm_config_filepath: PathBuf,

    /// The absolute filepath of `clang`
    clang_filepath: PathBuf,

    /// The absolute filepath of `clang++`
    clangxx_filepath: PathBuf,

    /// The absolute filepath of `llvm-ar`
    llvm_ar_filepath: PathBuf,

    /// The absolute filepath of `llvm-link`
    llvm_link_filepath: PathBuf,

    /// The absolute filepath of `llvm-objcopy` (optional, currently unused)
    llvm_objcopy_filepath: Option<PathBuf>,

    /// The absolute path of the directory that stores intermediate bitcode files
    bitcode_store_path: Option<PathBuf>,

    /// Extra user-provided linking flags for `llvm-link`
    llvm_link_flags: Option<Vec<String>>,

    /// Extra user-provided linking flags for link time optimization
    lto_ldflags: Option<Vec<String>>,

    /// Extra user-provided flags for bitcode generation, e.g., "-flto -fwhole-program-vtables"
    bitcode_generation_flags: Option<Vec<String>>,

    /// The configure only mode, which skips the bitcode generation (Default: false)
    is_configure_only: Option<bool>,

    /// Log level (Default: 0, print nothing)
    log_level: Option<u8>,

    /// Enable incremental bitcode caching (Default: false).
    /// Can also be enabled via `RLLVM_CACHE=1` environment variable.
    cache_enabled: Option<bool>,

    /// Custom cache directory path (Default: `~/.rllvm/cache/`)
    cache_dir: Option<PathBuf>,
}

impl RLLVMConfig {
    /// Returns the path to `llvm-config`.
    pub fn llvm_config_filepath(&self) -> &PathBuf {
        &self.llvm_config_filepath
    }

    /// Returns the path to `clang`.
    pub fn clang_filepath(&self) -> &PathBuf {
        &self.clang_filepath
    }

    /// Returns the path to `clang++`.
    pub fn clangxx_filepath(&self) -> &PathBuf {
        &self.clangxx_filepath
    }

    /// Returns the path to `llvm-ar`.
    pub fn llvm_ar_filepath(&self) -> &PathBuf {
        &self.llvm_ar_filepath
    }

    /// Returns the path to `llvm-link`.
    pub fn llvm_link_filepath(&self) -> &PathBuf {
        &self.llvm_link_filepath
    }

    /// Returns the optional path to `llvm-objcopy`.
    pub fn llvm_objcopy_filepath(&self) -> Option<&PathBuf> {
        self.llvm_objcopy_filepath.as_ref()
    }

    /// Returns the optional bitcode store directory path.
    pub fn bitcode_store_path(&self) -> Option<&PathBuf> {
        self.bitcode_store_path.as_ref()
    }

    /// Returns the optional extra flags for `llvm-link`.
    pub fn llvm_link_flags(&self) -> Option<&Vec<String>> {
        self.llvm_link_flags.as_ref()
    }

    /// Returns the optional LTO link flags.
    pub fn lto_ldflags(&self) -> Option<&Vec<String>> {
        self.lto_ldflags.as_ref()
    }

    /// Returns the optional bitcode generation flags.
    pub fn bitcode_generation_flags(&self) -> Option<&Vec<String>> {
        self.bitcode_generation_flags.as_ref()
    }

    /// Returns whether configure-only mode is enabled (skips bitcode generation).
    pub fn is_configure_only(&self) -> bool {
        self.is_configure_only.unwrap_or_default()
    }

    /// Returns the configured log level.
    pub fn log_level(&self) -> Level {
        match self.log_level.unwrap_or_default() {
            0 => Level::ERROR,
            1 => Level::WARN,
            2 => Level::INFO,
            3 => Level::DEBUG,
            _ => Level::TRACE,
        }
    }

    /// Returns whether caching is enabled in the config.
    pub fn cache_enabled(&self) -> bool {
        self.cache_enabled.unwrap_or_default()
    }

    /// Returns the optional custom cache directory path.
    pub fn cache_dir(&self) -> Option<&PathBuf> {
        self.cache_dir.as_ref()
    }
}

impl RLLVMConfig {
    /// Loads configuration from the config file.
    ///
    /// The file path is determined by the `RLLVM_CONFIG` environment variable,
    /// falling back to `~/.rllvm/config.toml`.
    pub fn new() -> Result<Self, Error> {
        Self::load_path(config_filepath())
    }

    fn load_path<P>(config_filepath: P) -> Result<Self, Error>
    where
        P: AsRef<Path> + std::fmt::Debug,
    {
        let config_filepath = config_filepath.as_ref();

        // An existing file is parsed; otherwise the configuration is inferred
        // from the LLVM installation and written out for next time.
        //
        // This is deliberately not `confy::load_path`, which reaches for
        // `Default` to create a missing file. Inferring a configuration can
        // fail (no `llvm-config` on the system), and `Default` has no way to
        // report that other than panicking — on the very first run, at that.
        let mut config = if Self::config_file_has_content(config_filepath) {
            Self::parse_file(config_filepath)?
        } else {
            let inferred = Self::try_default()?;
            inferred.write_to(config_filepath)?;
            inferred
        };

        config.validate_tool_paths();

        if let Some(bitcode_store_path) = &config.bitcode_store_path {
            // Check if the bitcode store path is absolute or not
            if !bitcode_store_path.is_absolute() {
                // Not absolute
                tracing::warn!(
                    "Ignore the bitcode store path, as it is not absolute: {:?}",
                    bitcode_store_path
                );
                config.bitcode_store_path = None;
            } else {
                // Further check if the directory exists
                if !bitcode_store_path.exists() {
                    // Not exist, then create it
                    tracing::info!(
                        "Create the directory for the bitcode store: {:?}",
                        bitcode_store_path
                    );
                    fs::create_dir_all(bitcode_store_path).map_err(|err| {
                        tracing::error!(
                            "Failed to create the bitcode store directory: err={}",
                            err
                        );
                        err
                    })?;
                } else {
                    // Finally, check if this is a directory
                    if !bitcode_store_path.is_dir() {
                        // Not a directory
                        tracing::warn!(
                            "Ignore the bitcode store path, as it is not a directory: {:?}",
                            bitcode_store_path
                        );
                        config.bitcode_store_path = None;
                    }
                }
            }
        }

        Ok(config)
    }
}

impl RLLVMConfig {
    /// Returns `true` if the path names a file with something in it.
    ///
    /// An empty file is treated as absent, matching how a partially written or
    /// truncated config would otherwise fail to parse.
    fn config_file_has_content(config_filepath: &Path) -> bool {
        fs::metadata(config_filepath).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    }

    /// Parse a configuration file from disk.
    fn parse_file(config_filepath: &Path) -> Result<Self, Error> {
        let contents = fs::read_to_string(config_filepath).map_err(|err| {
            tracing::error!(
                "Failed to read configuration: config_filepath={:?}, err={}",
                config_filepath,
                err
            );
            Error::ConfigError(format!(
                "Failed to read configuration from {config_filepath:?}: {err}"
            ))
        })?;

        toml::from_str(&contents).map_err(|err| {
            tracing::error!(
                "Failed to parse configuration: config_filepath={:?}, err={}",
                config_filepath,
                err
            );
            Error::ConfigError(format!(
                "Failed to parse configuration from {config_filepath:?}: {err}"
            ))
        })
    }

    /// Serialize this configuration to the given path, creating parent
    /// directories as needed.
    fn write_to(&self, config_filepath: &Path) -> Result<(), Error> {
        if let Some(parent_dir) = config_filepath.parent()
            && !parent_dir.as_os_str().is_empty()
        {
            fs::create_dir_all(parent_dir)?;
        }

        let contents = toml::to_string_pretty(self).map_err(|err| {
            Error::ConfigError(format!("Failed to serialize the configuration: {err}"))
        })?;
        fs::write(config_filepath, contents).map_err(|err| {
            tracing::error!(
                "Failed to write configuration: config_filepath={:?}, err={}",
                config_filepath,
                err
            );
            err
        })?;

        tracing::info!("Wrote inferred configuration to {:?}", config_filepath);
        Ok(())
    }
}

impl RLLVMConfig {
    /// Checks that configured tool paths exist on disk, printing colored errors for each missing tool.
    fn validate_tool_paths(&self) {
        let tools: &[(&str, &Path)] = &[
            ("llvm-config", &self.llvm_config_filepath),
            ("clang", &self.clang_filepath),
            ("clang++", &self.clangxx_filepath),
            ("llvm-ar", &self.llvm_ar_filepath),
            ("llvm-link", &self.llvm_link_filepath),
        ];

        for (name, path) in tools {
            if !path.exists() {
                print_missing_tool_error(name, Some(path));
            }
        }

        // `llvm-objcopy` is optional: no code path invokes it today, so a stale
        // or absent entry must not be reported as an error.
        if let Some(llvm_objcopy_filepath) = &self.llvm_objcopy_filepath
            && !llvm_objcopy_filepath.exists()
        {
            tracing::debug!(
                "Configured `llvm-objcopy` does not exist: {:?}",
                llvm_objcopy_filepath
            );
        }

        // Check version compatibility between clang and LLVM tools
        if self.clang_filepath.exists() && self.llvm_config_filepath.exists() {
            check_version_compatibility(&self.clang_filepath, &self.llvm_config_filepath);
        }
    }

    /// Infers configuration by discovering LLVM tools on the system.
    ///
    /// Uses [`find_llvm_config`](crate::utils::find_llvm_config) to locate
    /// `llvm-config`, then derives all other tool paths from `llvm-config --bindir`.
    pub fn try_default() -> Result<Self, Error> {
        tracing::info!("Infer rllvm configurations ...");

        // Find `llvm-config`
        let llvm_config_filepath = find_llvm_config().inspect_err(|_| {
            print_missing_tool_error("llvm-config", None);
        })?;
        tracing::info!("- llvm-config: {:?}", llvm_config_filepath);

        // Obtain LLVM version
        match execute_llvm_config(&llvm_config_filepath, &["--version"]) {
            Ok(llvm_version) => tracing::info!("- LLVM version: {}", llvm_version),
            Err(err) => tracing::warn!("- LLVM version: (unknown, err={:?})", err),
        }

        let llvm_bindir = PathBuf::from(
            execute_llvm_config(&llvm_config_filepath, &["--bindir"]).map_err(|err| {
                tracing::error!("Failed to execute `llvm-config --bindir`: {:?}", err);
                err
            })?,
        );

        // Find `clang`
        let clang_filepath = llvm_bindir.join("clang");

        // Find `clang++`
        let clangxx_filepath = llvm_bindir.join("clang++");

        // Find `llvm-ar`
        let llvm_ar_filepath = llvm_bindir.join("llvm-ar");

        // Find `llvm-link`
        let llvm_link_filepath = llvm_bindir.join("llvm-link");

        // Find `llvm-objcopy`, which is optional: it is recorded when present,
        // but nothing invokes it, so its absence must not fail the inference.
        let llvm_objcopy_filepath = llvm_bindir.join("llvm-objcopy");
        let llvm_objcopy_filepath = if llvm_objcopy_filepath.exists() {
            Some(llvm_objcopy_filepath)
        } else {
            tracing::debug!("- llvm-objcopy: (not found in {:?})", llvm_bindir);
            None
        };

        let llvm_bin_tools: &[(&str, &PathBuf)] = &[
            ("clang", &clang_filepath),
            ("clang++", &clangxx_filepath),
            ("llvm-ar", &llvm_ar_filepath),
            ("llvm-link", &llvm_link_filepath),
        ];
        for (name, filepath) in llvm_bin_tools {
            if !filepath.exists() {
                print_missing_tool_error(name, Some(filepath));
                return Err(Error::MissingFile(format!("{filepath:?}")));
            }
        }

        // Check version compatibility between clang and LLVM tools
        check_version_compatibility(&clang_filepath, &llvm_config_filepath);

        Ok(Self {
            llvm_config_filepath,
            clang_filepath,
            clangxx_filepath,
            llvm_ar_filepath,
            llvm_link_filepath,
            llvm_objcopy_filepath,
            bitcode_store_path: None,
            llvm_link_flags: None,
            lto_ldflags: None,
            bitcode_generation_flags: None,
            is_configure_only: None,
            log_level: None,
            cache_enabled: None,
            cache_dir: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a config file containing the required tool paths plus `extra`,
    /// returning the owning temporary directory, its path, and the inferred
    /// configuration the paths came from.
    fn write_config(extra: &str) -> (tempfile::TempDir, PathBuf, RLLVMConfig) {
        let inferred = RLLVMConfig::try_default().expect("Failed to infer the LLVM tool paths");
        let contents = format!(
            "llvm_config_filepath = '{}'\n\
             clang_filepath = '{}'\n\
             clangxx_filepath = '{}'\n\
             llvm_ar_filepath = '{}'\n\
             llvm_link_filepath = '{}'\n\
             {}",
            inferred.llvm_config_filepath().display(),
            inferred.clang_filepath().display(),
            inferred.clangxx_filepath().display(),
            inferred.llvm_ar_filepath().display(),
            inferred.llvm_link_filepath().display(),
            extra,
        );

        let dir = tempfile::tempdir().expect("Failed to create a temporary directory");
        let config_filepath = dir.path().join("config.toml");
        fs::write(&config_filepath, contents).expect("Failed to write the test config file");
        (dir, config_filepath, inferred)
    }

    #[test]
    fn test_load_config_without_llvm_objcopy_filepath() {
        let (_dir, config_filepath, inferred) = write_config("");

        let config = RLLVMConfig::load_path(&config_filepath)
            .expect("A config without `llvm_objcopy_filepath` should load");

        assert!(config.llvm_objcopy_filepath().is_none());
        assert_eq!(config.clang_filepath(), inferred.clang_filepath());
        assert_eq!(config.llvm_link_filepath(), inferred.llvm_link_filepath());
    }

    #[test]
    fn test_load_config_with_llvm_objcopy_filepath() {
        // Existing config files still set the key; they must keep loading.
        let llvm_objcopy_filepath = RLLVMConfig::try_default()
            .expect("Failed to infer the LLVM tool paths")
            .llvm_objcopy_filepath()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("llvm-objcopy"));
        let (_dir, config_filepath, _inferred) = write_config(&format!(
            "llvm_objcopy_filepath = '{}'\n",
            llvm_objcopy_filepath.display()
        ));

        let config = RLLVMConfig::load_path(&config_filepath)
            .expect("A config with `llvm_objcopy_filepath` should load");

        assert_eq!(config.llvm_objcopy_filepath(), Some(&llvm_objcopy_filepath));
    }
}
