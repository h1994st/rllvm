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
        BITCODE_ROOT_ENV_NAME, DEFAULT_CONF_FILEPATH_UNDER_HOME, DEFAULT_QUERY_CACHE_WARN_MB,
        DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME, HOME_ENV_NAME, LOG_LEVEL_ENV_NAME, LTO_MODE_ENV_NAME,
        QUERY_CACHE_ENV_NAME, RUSTC_ENV_NAME,
    },
    diagnostics::{check_version_compatibility, print_missing_tool_error},
    error::Error,
    lto::LtoMode,
    utils::{execute_llvm_config, find_llvm_config},
};

/// The configuration key naming the directory of the unnamed LLVM tools.
const LLVM_BINDIR_KEY: &str = "llvm_bindir";

/// The `llvm-config` argument that reports its bindir.
const LLVM_CONFIG_BINDIR_ARG: &str = "--bindir";

/// The cached outcome of loading the configuration.
///
/// The failure is stored as a message rather than as an [`Error`], because a
/// `OnceLock` hands out shared references and the error type is not `Clone` — every
/// caller needs its own owned error.
type ConfigResult = Result<RLLVMConfig, String>;

/// The process-wide configuration, resolved once by whichever entry point
/// reaches it first.
static RLLVM_CONFIG: OnceLock<ConfigResult> = OnceLock::new();

fn config_result_to_ref(result: &'static ConfigResult) -> Result<&'static RLLVMConfig, Error> {
    match result {
        Ok(config) => Ok(config),
        Err(message) => Err(Error::ConfigError(message.clone())),
    }
}

/// Pins the process-wide configuration to one inferred from the system.
///
/// [`try_rllvm_config`] otherwise reads -- and on a first run writes --
/// `~/.rllvm/config.toml`, which a test must never depend on or modify. This
/// crate's own unit tests get the inferred configuration from the `cfg(test)`
/// variant below; a unit test in a crate that depends on this one compiles
/// against the ordinary variant and has to ask for it here instead.
///
/// Both share one `OnceLock`, so this pins nothing once a configuration has
/// been resolved: a test has to call it before anything builds a wrapper.
#[doc(hidden)]
pub fn pin_inferred_config() -> Result<&'static RLLVMConfig, Error> {
    config_result_to_ref(RLLVM_CONFIG.get_or_init(|| {
        RLLVMConfig::try_default()
            .map_err(|err| format!("Failed to infer rllvm configuration: {err}"))
    }))
}

#[cfg(not(test))]
pub fn try_rllvm_config() -> Result<&'static RLLVMConfig, Error> {
    config_result_to_ref(RLLVM_CONFIG.get_or_init(|| {
        RLLVMConfig::new().map_err(|err| format!("Failed to load rllvm configuration: {err}"))
    }))
}

/// Returns the global [`RLLVMConfig`] singleton (test variant), inferred from
/// the system so this crate's own tests never read the user's configuration.
#[cfg(test)]
pub fn try_rllvm_config() -> Result<&'static RLLVMConfig, Error> {
    pin_inferred_config()
}

/// Resolve a configured bitcode root to the form paths are compared against.
///
/// The root is matched against each bitcode file's real path, so a root that
/// reaches rllvm through a symlink -- `/tmp` and `/var` both are on macOS --
/// would otherwise match nothing and the paths would be recorded absolute
/// with no warning. Callers should not have to pass a canonical path.
///
/// A root that does not exist yet is kept as given: `canonicalize` fails on a
/// missing path, and a build may create the directory later.
fn normalize_root(root: PathBuf) -> PathBuf {
    root.canonicalize().unwrap_or(root)
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

    /// The absolute path of the directory holding the LLVM tools the
    /// configuration does not name (Default: what `llvm-config --bindir`
    /// reports)
    llvm_bindir: Option<PathBuf>,

    /// The absolute filepath of `rustc` (optional; `which rustc` when unset)
    rustc_filepath: Option<PathBuf>,

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

    /// Root that embedded bitcode paths are recorded relative to (Default: none)
    bitcode_root: Option<PathBuf>,

    /// How to handle `-flto` builds: `marker`, `save-temps` or `skip`
    /// (Default: `marker`)
    lto_mode: Option<LtoMode>,

    /// Custom cache directory path (Default: `~/.rllvm/cache/`)
    cache_dir: Option<PathBuf>,

    /// Persist rllvm-query's extracted per-module facts (Default: true).
    /// `RLLVM_QUERY_CACHE=0|1` overrides.
    query_cache: Option<bool>,

    /// Disk use of the query facts cache, in MiB, past which rllvm-query
    /// warns (Default: 1024).
    query_cache_warn_mb: Option<u64>,

    /// `llvm_bindir`, or else what `llvm-config --bindir` reported, decided
    /// on first use so a process asks `llvm-config` at most once.
    #[serde(skip)]
    resolved_llvm_bindir: OnceLock<Result<LlvmBindir, String>>,
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

    /// Returns the directory holding the LLVM tools the configuration does
    /// not name, such as `llvm-nm`, `llvm-dis` and `llvm-extract`.
    ///
    /// That is `llvm_bindir` when the configuration sets it, and otherwise
    /// what the configured `llvm-config --bindir` reports. `llvm-config` runs
    /// at most once per configuration, and not at all when the key is set.
    /// There is no other place these tools are looked for: not beside another
    /// configured tool, and not in a discovered LLVM.
    pub fn llvm_bindir(&self) -> Result<&LlvmBindir, Error> {
        self.resolved_llvm_bindir
            .get_or_init(|| match &self.llvm_bindir {
                // A relative one would mean a different directory in each
                // working directory a build runs a tool from.
                Some(path) if path.is_relative() => Err(format!(
                    "`{LLVM_BINDIR_KEY}` must be an absolute path, not {}",
                    path.display()
                )),
                Some(path) => Ok(LlvmBindir::configured(path)),
                None => LlvmBindir::reported_by(&self.llvm_config_filepath),
            })
            .as_ref()
            .map_err(|message| Error::ConfigError(message.clone()))
    }

    /// Returns the LLVM tool `name` from [`llvm_bindir`](Self::llvm_bindir).
    ///
    /// Fails with [`Error::MissingFile`] naming the tool, the directory, and
    /// where that directory came from when the tool is not there.
    pub fn llvm_tool(&self, name: &str) -> Result<PathBuf, Error> {
        self.llvm_bindir()?.tool(name)
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
    /// Returns the log level.
    ///
    /// `$RLLVM_LOG_LEVEL` wins over the configuration file. `rllvm-cc` layers
    /// `--rllvm-verbose` on top of both; `rllvm-rustc` has no such flag,
    /// because cargo owns its command line.
    pub fn log_level(&self) -> Level {
        let level = env::var(LOG_LEVEL_ENV_NAME)
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_else(|| self.log_level.unwrap_or_default());
        match level {
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

    /// Returns the root that embedded bitcode paths are recorded relative to.
    ///
    /// `$RLLVM_BITCODE_ROOT` wins over the configuration file, so a build can opt
    /// into relocatable paths without editing a shared config.
    ///
    /// When unset, paths are recorded absolute, which is the historical
    /// behaviour and keeps existing objects readable.
    pub fn bitcode_root(&self) -> Option<PathBuf> {
        env::var(BITCODE_ROOT_ENV_NAME)
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.bitcode_root.clone())
            .map(normalize_root)
    }

    /// Returns the configured `rustc`, if any.
    ///
    /// `$RLLVM_REAL_RUSTC` wins over the configuration file. Unset here and in
    /// the environment, the wrapper falls back to `rustc` on `PATH`.
    pub fn rustc_filepath(&self) -> Option<PathBuf> {
        env::var(RUSTC_ENV_NAME)
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.rustc_filepath.clone())
    }

    /// Returns the configured LTO mode.
    ///
    /// `$RLLVM_LTO_MODE` wins over the configuration file, so one build can
    /// switch modes without editing a config other builds share.
    ///
    /// An unrecognised value is an error rather than a fallback: silently
    /// producing a binary nothing can be extracted from is the bug #96 exists
    /// to fix.
    pub fn lto_mode(&self) -> Result<LtoMode, Error> {
        match env::var(LTO_MODE_ENV_NAME) {
            Ok(value) if !value.is_empty() => value.parse(),
            _ => Ok(self.lto_mode.unwrap_or_default()),
        }
    }

    /// Returns the optional custom cache directory path.
    pub fn cache_dir(&self) -> Option<&PathBuf> {
        self.cache_dir.as_ref()
    }

    /// Whether rllvm-query reads and writes its facts cache.
    /// `$RLLVM_QUERY_CACHE` (`0` or `1`) wins over the configuration file.
    pub fn query_cache_enabled(&self) -> bool {
        query_cache_override(env::var(QUERY_CACHE_ENV_NAME).ok().as_deref())
            .unwrap_or_else(|| self.query_cache_enabled_ignoring_env())
    }

    fn query_cache_enabled_ignoring_env(&self) -> bool {
        self.query_cache.unwrap_or(true)
    }

    /// The facts cache size, in bytes, past which rllvm-query warns.
    pub fn query_cache_warn_bytes(&self) -> u64 {
        self.query_cache_warn_mb
            .unwrap_or(DEFAULT_QUERY_CACHE_WARN_MB)
            .saturating_mul(1024 * 1024)
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
    /// Uses [`find_llvm_config`] to locate
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
            execute_llvm_config(&llvm_config_filepath, &[LLVM_CONFIG_BINDIR_ARG]).map_err(
                |err| {
                    tracing::error!("Failed to execute `llvm-config --bindir`: {:?}", err);
                    err
                },
            )?,
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
            // Recorded so that a written configuration never has to ask
            // `llvm-config` again.
            llvm_bindir: Some(llvm_bindir),
            // Not inferred: rustc is not an LLVM tool and need not be
            // installed. The wrapper falls back to `rustc` on `PATH`.
            rustc_filepath: None,
            bitcode_store_path: None,
            llvm_link_flags: None,
            lto_ldflags: None,
            lto_mode: None,
            bitcode_generation_flags: None,
            is_configure_only: None,
            log_level: None,
            bitcode_root: None,
            cache_enabled: None,
            cache_dir: None,
            query_cache: None,
            query_cache_warn_mb: None,
            resolved_llvm_bindir: OnceLock::new(),
        })
    }
}

/// The directory holding the LLVM tools the configuration does not name,
/// and how it was decided, so a tool missing from it can say where to fix
/// that.
///
/// Obtained from [`RLLVMConfig::llvm_bindir`], or made with
/// [`LlvmBindir::configured`] by a caller that has its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlvmBindir {
    path: PathBuf,
    /// The `llvm-config` that reported `path`; `None` when it is configured.
    reported_by: Option<PathBuf>,
}

impl LlvmBindir {
    /// A bindir named by configuration, as the `llvm_bindir` key does.
    pub fn configured(path: impl Into<PathBuf>) -> LlvmBindir {
        LlvmBindir {
            path: path.into(),
            reported_by: None,
        }
    }

    /// The bindir `llvm_config --bindir` reports. The failure is a message,
    /// because it is cached and [`Error`] is not `Clone`.
    fn reported_by(llvm_config: &Path) -> Result<LlvmBindir, String> {
        let unknown = |reason: String| {
            format!(
                "Cannot find the LLVM bindir: `{} {LLVM_CONFIG_BINDIR_ARG}` {reason}; set \
                 `{LLVM_BINDIR_KEY}` in the configuration to the directory holding the LLVM tools",
                llvm_config.display()
            )
        };
        let answer = execute_llvm_config(llvm_config, &[LLVM_CONFIG_BINDIR_ARG])
            .map_err(|err| unknown(format!("failed: {err}")))?;
        // An empty answer would otherwise name a file in the working directory.
        if answer.is_empty() {
            return Err(unknown("reported nothing".to_string()));
        }
        Ok(LlvmBindir {
            path: PathBuf::from(answer),
            reported_by: Some(llvm_config.to_path_buf()),
        })
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tool `name` in this directory, or [`Error::MissingFile`] naming
    /// the tool, the directory, and where the directory came from.
    pub fn tool(&self, name: &str) -> Result<PathBuf, Error> {
        let tool = self.path.join(name);
        if tool.is_file() {
            return Ok(tool);
        }
        let origin = match &self.reported_by {
            None => format!("configured as `{LLVM_BINDIR_KEY}`"),
            Some(llvm_config) => format!(
                "reported by `{} {LLVM_CONFIG_BINDIR_ARG}`; set `{LLVM_BINDIR_KEY}` in the \
                 configuration to the directory that holds it",
                llvm_config.display()
            ),
        };
        Err(Error::MissingFile(format!(
            "`{name}` is not in the LLVM bindir {}, {origin}",
            self.path.display()
        )))
    }
}

/// `$RLLVM_QUERY_CACHE` as an override: `0` and `1` decide, anything else
/// defers to the configuration file.
fn query_cache_override(value: Option<&str>) -> Option<bool> {
    match value {
        Some("0") => Some(false),
        Some("1") => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lto::LtoMode;

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
    fn bitcode_store_path_relative_is_ignored() {
        let (_dir, config_filepath, _) = write_config("bitcode_store_path = 'relative/dir'\n");
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");
        assert!(
            config.bitcode_store_path().is_none(),
            "a relative bitcode store path must be ignored"
        );
    }

    #[test]
    fn bitcode_store_path_absolute_is_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store").join("nested");
        assert!(!store.exists());

        let (_cfg_dir, config_filepath, _) =
            write_config(&format!("bitcode_store_path = '{}'\n", store.display()));
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert_eq!(config.bitcode_store_path(), Some(&store));
        assert!(store.is_dir(), "the store directory was not created");
    }

    #[test]
    fn bitcode_store_path_pointing_at_a_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("a_file");
        fs::write(&not_a_dir, b"x").unwrap();

        let (_cfg_dir, config_filepath, _) =
            write_config(&format!("bitcode_store_path = '{}'\n", not_a_dir.display()));
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert!(
            config.bitcode_store_path().is_none(),
            "a store path that is not a directory must be ignored"
        );
    }

    #[test]
    fn bitcode_store_path_existing_directory_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        fs::create_dir_all(&store).unwrap();

        let (_cfg_dir, config_filepath, _) =
            write_config(&format!("bitcode_store_path = '{}'\n", store.display()));
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");
        assert_eq!(config.bitcode_store_path(), Some(&store));
    }

    #[test]
    fn missing_config_file_is_written_from_inferred_values() {
        let dir = tempfile::tempdir().unwrap();
        let config_filepath = dir.path().join("nested").join("config.toml");
        assert!(!config_filepath.exists());

        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert!(
            config_filepath.exists(),
            "first run must write the inferred config"
        );
        assert!(config.clang_filepath().exists());
    }

    #[test]
    fn optional_flag_accessors_round_trip() {
        let (_dir, config_filepath, _) = write_config(
            "llvm_link_flags = ['-v']\n\
             lto_ldflags = ['-flto']\n\
             bitcode_generation_flags = ['-g']\n\
             is_configure_only = true\n\
             cache_enabled = true\n\
             log_level = 3\n",
        );
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert_eq!(config.llvm_link_flags(), Some(&vec!["-v".to_string()]));
        assert_eq!(config.lto_ldflags(), Some(&vec!["-flto".to_string()]));
        assert_eq!(
            config.bitcode_generation_flags(),
            Some(&vec!["-g".to_string()])
        );
        assert!(config.is_configure_only());
        assert!(config.cache_enabled());
        assert_eq!(config.log_level(), Level::DEBUG);
    }

    #[test]
    fn log_level_mapping_covers_every_value() {
        for (value, expected) in [
            (0u8, Level::ERROR),
            (1, Level::WARN),
            (2, Level::INFO),
            (3, Level::DEBUG),
            (4, Level::TRACE),
            (9, Level::TRACE),
        ] {
            let (_dir, config_filepath, _) = write_config(&format!("log_level = {value}\n"));
            let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");
            assert_eq!(config.log_level(), expected, "log_level = {value}");
        }
    }

    #[test]
    fn load_config_without_llvm_objcopy_filepath() {
        let (_dir, config_filepath, inferred) = write_config("");

        let config = RLLVMConfig::load_path(&config_filepath)
            .expect("A config without `llvm_objcopy_filepath` should load");

        assert!(config.llvm_objcopy_filepath().is_none());
        assert_eq!(config.clang_filepath(), inferred.clang_filepath());
        assert_eq!(config.llvm_link_filepath(), inferred.llvm_link_filepath());
    }

    #[test]
    fn lto_mode_is_read_from_the_config_file() {
        let (_dir, config_filepath, _) = write_config("lto_mode = 'save-temps'\n");
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert_eq!(config.lto_mode().unwrap(), LtoMode::SaveTemps);
    }

    #[test]
    fn lto_mode_defaults_to_marker_when_absent() {
        let (_dir, config_filepath, _) = write_config("log_level = 0\n");
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        assert_eq!(config.lto_mode().unwrap(), LtoMode::Marker);
    }

    #[test]
    fn load_config_with_llvm_objcopy_filepath() {
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

    #[test]
    fn the_query_cache_is_on_with_a_one_gib_warning_by_default() {
        let (_dir, config_filepath, _) = write_config("");
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");
        assert!(config.query_cache_enabled_ignoring_env());
        assert_eq!(config.query_cache_warn_bytes(), 1024 * 1024 * 1024);
    }

    #[test]
    fn the_query_cache_keys_are_read_from_the_config_file() {
        let (_dir, config_filepath, _) =
            write_config("query_cache = false\nquery_cache_warn_mb = 5\n");
        let config = RLLVMConfig::load_path(&config_filepath).expect("load failed");
        assert!(!config.query_cache_enabled_ignoring_env());
        assert_eq!(config.query_cache_warn_bytes(), 5 * 1024 * 1024);
    }

    /// A configuration whose required tool paths are placeholders, plus
    /// `extra`. Parsed without loading, so nothing validates or runs a tool.
    fn config_from(llvm_config: &Path, extra: &str) -> RLLVMConfig {
        toml::from_str(&format!(
            "llvm_config_filepath = '{}'\n\
             clang_filepath = '/unused/clang'\n\
             clangxx_filepath = '/unused/clang++'\n\
             llvm_ar_filepath = '/unused/llvm-ar'\n\
             llvm_link_filepath = '/unused/llvm-link'\n\
             {extra}",
            llvm_config.display()
        ))
        .expect("Failed to parse the test configuration")
    }

    /// An executable `llvm-config` stand-in in `dir` that appends its
    /// arguments to `log` and then runs `body`.
    #[cfg(unix)]
    fn stand_in_llvm_config(dir: &Path, log: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("llvm-config");
        fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
    fn a_configured_llvm_bindir_is_used_without_running_llvm_config() {
        let dir = tempfile::tempdir().unwrap();
        let bindir = dir.path().join("bin");
        fs::create_dir(&bindir).unwrap();
        fs::write(bindir.join("llvm-nm"), b"").unwrap();
        let log = dir.path().join("llvm-config.log");
        let llvm_config = stand_in_llvm_config(dir.path(), &log, "exit 1");

        let config = config_from(
            &llvm_config,
            &format!("llvm_bindir = '{}'\n", bindir.display()),
        );

        assert_eq!(config.llvm_tool("llvm-nm").unwrap(), bindir.join("llvm-nm"));
        assert!(!log.exists(), "llvm-config ran: {:?}", fs::read(&log));
    }

    #[test]
    #[cfg(unix)]
    fn an_unset_llvm_bindir_asks_the_configured_llvm_config_once() {
        let dir = tempfile::tempdir().unwrap();
        let reported = dir.path().join("reported");
        fs::create_dir(&reported).unwrap();
        fs::write(reported.join("llvm-dis"), b"").unwrap();
        fs::write(reported.join("llvm-nm"), b"").unwrap();
        let log = dir.path().join("llvm-config.log");
        let llvm_config =
            stand_in_llvm_config(dir.path(), &log, &format!("echo '{}'", reported.display()));

        let config = config_from(&llvm_config, "");

        assert_eq!(
            config.llvm_tool("llvm-dis").unwrap(),
            reported.join("llvm-dis")
        );
        assert_eq!(
            config.llvm_tool("llvm-nm").unwrap(),
            reported.join("llvm-nm")
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "--bindir\n");
    }

    #[test]
    fn a_tool_missing_from_the_configured_llvm_bindir_is_named_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_from(
            Path::new("/unused/llvm-config"),
            &format!("llvm_bindir = '{}'\n", dir.path().display()),
        );

        match config.llvm_tool("llvm-extract") {
            Err(Error::MissingFile(message)) => {
                for expected in [
                    "llvm-extract",
                    &dir.path().display().to_string(),
                    "llvm_bindir",
                ] {
                    assert!(
                        message.contains(expected),
                        "{expected} not named: {message}"
                    );
                }
            }
            other => panic!("expected a missing file, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_tool_missing_from_the_reported_bindir_names_the_llvm_config() {
        let dir = tempfile::tempdir().unwrap();
        let reported = dir.path().join("reported");
        fs::create_dir(&reported).unwrap();
        let log = dir.path().join("llvm-config.log");
        let llvm_config =
            stand_in_llvm_config(dir.path(), &log, &format!("echo '{}'", reported.display()));
        let config = config_from(&llvm_config, "");

        match config.llvm_tool("llvm-nm") {
            Err(Error::MissingFile(message)) => {
                for expected in [
                    "llvm-nm",
                    &reported.display().to_string(),
                    &format!("{} --bindir", llvm_config.display()),
                ] {
                    assert!(
                        message.contains(expected),
                        "{expected} not named: {message}"
                    );
                }
            }
            other => panic!("expected a missing file, got {other:?}"),
        }
    }

    /// A relative bindir would resolve against each process's working
    /// directory, and so differ across one build.
    #[test]
    fn a_relative_llvm_bindir_is_rejected() {
        let config = config_from(
            Path::new("/unused/llvm-config"),
            "llvm_bindir = 'llvm/bin'\n",
        );
        match config.llvm_tool("llvm-nm") {
            Err(Error::ConfigError(message)) => {
                for expected in ["llvm_bindir", "llvm/bin", "absolute"] {
                    assert!(
                        message.contains(expected),
                        "{expected} not named: {message}"
                    );
                }
            }
            other => panic!("expected a configuration error, got {other:?}"),
        }
    }

    /// An empty answer would otherwise name a file in the working directory.
    #[test]
    #[cfg(unix)]
    fn an_empty_bindir_answer_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("llvm-config.log");
        let llvm_config = stand_in_llvm_config(dir.path(), &log, "echo");
        let config = config_from(&llvm_config, "");

        let error = config.llvm_tool("llvm-nm").unwrap_err().to_string();
        assert!(error.contains("--bindir"), "{error}");
        assert!(error.contains("llvm_bindir"), "{error}");
    }

    #[test]
    fn an_inferred_configuration_records_its_llvm_bindir() {
        let dir = tempfile::tempdir().unwrap();
        let config_filepath = dir.path().join("config.toml");
        let inferred = RLLVMConfig::load_path(&config_filepath).expect("load failed");

        let written = RLLVMConfig::parse_file(&config_filepath).unwrap();
        let bindir = written
            .llvm_bindir
            .clone()
            .expect("the written configuration has no llvm_bindir");
        assert_eq!(Some(bindir.as_path()), inferred.clang_filepath().parent());
    }

    #[test]
    fn the_environment_overrides_the_query_cache_switch() {
        assert_eq!(query_cache_override(Some("0")), Some(false));
        assert_eq!(query_cache_override(Some("1")), Some(true));
        assert_eq!(query_cache_override(Some("")), None);
        assert_eq!(query_cache_override(Some("yes")), None);
        assert_eq!(query_cache_override(None), None);
    }
}
