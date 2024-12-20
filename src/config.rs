use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use log::Level;
use serde::{Deserialize, Serialize};

use crate::{
    constants::{
        DEFAULT_CONF_FILEPATH_UNDER_HOME, DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME, HOME_ENV_NAME,
    },
    utils::{execute_llvm_config, find_llvm_config},
};

#[cfg(not(test))]
pub fn rllvm_config() -> &'static RLLVMConfig {
    static RLLVM_CONFIG: OnceLock<RLLVMConfig> = OnceLock::new();
    RLLVM_CONFIG.get_or_init(|| RLLVMConfig::new())
}

#[cfg(test)]
pub fn rllvm_config() -> &'static RLLVMConfig {
    static RLLVM_CONFIG: OnceLock<RLLVMConfig> = OnceLock::new();
    RLLVM_CONFIG.get_or_init(|| RLLVMConfig::default())
}

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

    /// The absolute filepath of `llvm-objcopy`
    llvm_objcopy_filepath: PathBuf,

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
}

impl RLLVMConfig {
    pub fn llvm_config_filepath(&self) -> &PathBuf {
        &self.llvm_config_filepath
    }

    pub fn clang_filepath(&self) -> &PathBuf {
        &self.clang_filepath
    }

    pub fn clangxx_filepath(&self) -> &PathBuf {
        &self.clangxx_filepath
    }

    pub fn llvm_ar_filepath(&self) -> &PathBuf {
        &self.llvm_ar_filepath
    }

    pub fn llvm_link_filepath(&self) -> &PathBuf {
        &self.llvm_link_filepath
    }

    pub fn llvm_objcopy_filepath(&self) -> &PathBuf {
        &self.llvm_objcopy_filepath
    }

    pub fn bitcode_store_path(&self) -> Option<&PathBuf> {
        self.bitcode_store_path.as_ref()
    }

    pub fn llvm_link_flags(&self) -> Option<&Vec<String>> {
        self.llvm_link_flags.as_ref()
    }

    pub fn lto_ldflags(&self) -> Option<&Vec<String>> {
        self.lto_ldflags.as_ref()
    }

    pub fn bitcode_generation_flags(&self) -> Option<&Vec<String>> {
        self.bitcode_generation_flags.as_ref()
    }

    pub fn is_configure_only(&self) -> bool {
        self.is_configure_only.unwrap_or_default()
    }

    pub fn log_level(&self) -> Level {
        Level::iter()
            .nth(self.log_level.unwrap_or_default() as usize)
            .unwrap_or(Level::max())
    }
}

impl RLLVMConfig {
    pub fn new() -> Self {
        let config_filepath = env::var(DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME).map_or_else(
            |_| {
                // Default config file
                PathBuf::from(env::var(HOME_ENV_NAME).unwrap_or("".into()))
                    .join(DEFAULT_CONF_FILEPATH_UNDER_HOME)
            },
            |x| {
                // User-defined config file
                PathBuf::from(x)
            },
        );
        Self::load_path(config_filepath)
    }

    fn load_path<P>(config_filepath: P) -> Self
    where
        P: AsRef<Path> + std::fmt::Debug,
    {
        let config_filepath = config_filepath.as_ref();
        match confy::load_path::<RLLVMConfig>(config_filepath) {
            Ok(mut config) => {
                if let Some(bitcode_store_path) = &config.bitcode_store_path {
                    // Check if the bitcode store path is absolute or not
                    if !bitcode_store_path.is_absolute() {
                        // Not absolute
                        log::warn!(
                            "Ignore the bitcode store path, as it is not absolute: {:?}",
                            bitcode_store_path
                        );
                        config.bitcode_store_path = None;
                    } else {
                        // Further check if the directory exists
                        if !bitcode_store_path.exists() {
                            // Not exist, then create it
                            log::info!(
                                "Create the directory for the bitcode store: {:?}",
                                bitcode_store_path
                            );
                            if let Err(err) = fs::create_dir_all(bitcode_store_path) {
                                log::error!(
                                    "Failed to create the bitcode store directory: err={}",
                                    err
                                );
                                std::process::exit(1);
                            }
                        } else {
                            // Finally, check if this is a directory
                            if !bitcode_store_path.is_dir() {
                                // Not a directory
                                log::warn!(
                                    "Ignore the bitcode store path, as it is not a directory: {:?}",
                                    bitcode_store_path
                                );
                                config.bitcode_store_path = None;
                            }
                        }
                    }
                }

                config
            }
            Err(err) => {
                log::error!(
                    "Failed to load configuration: config_filepath={:?}, err={}",
                    config_filepath,
                    err
                );
                std::process::exit(1);
            }
        }
    }
}

impl Default for RLLVMConfig {
    fn default() -> Self {
        log::info!("Infer rllvm configurations ...");

        // Find `llvm-config`
        let llvm_config_filepath = find_llvm_config().unwrap_or_else(|err| {
            log::error!("Failed to find `llvm-config`: err={:?}", err);
            std::process::exit(1);
        });
        log::info!("- llvm-config: {:?}", llvm_config_filepath);

        // Obtain LLVM version
        match execute_llvm_config(&llvm_config_filepath, &["--version"]) {
            Ok(llvm_version) => log::info!("- LLVM version: {}", llvm_version),
            Err(err) => log::warn!("- LLVM version: (unknown, err={:?})", err),
        }

        let llvm_bindir = execute_llvm_config(&llvm_config_filepath, &["--bindir"]).map_or_else(
            |err| {
                log::error!("Failed to execute `llvm-config --bindir`: {:?}", err);
                std::process::exit(1);
            },
            |x| PathBuf::from(x),
        );

        // Find `clang`
        let clang_filepath = llvm_bindir.join("clang");

        // Find `clang++`
        let clangxx_filepath = llvm_bindir.join("clang++");

        // Find `llvm-ar`
        let llvm_ar_filepath = llvm_bindir.join("llvm-ar");

        // Find `llvm-link`
        let llvm_link_filepath = llvm_bindir.join("llvm-link");

        // Find `llvm-objcopy`
        let llvm_objcopy_filepath = llvm_bindir.join("llvm-objcopy");

        let llvm_bin_filepaths = [
            &clang_filepath,
            &clangxx_filepath,
            &llvm_ar_filepath,
            &llvm_link_filepath,
            &llvm_objcopy_filepath,
        ];
        for llvm_bin_filepath in llvm_bin_filepaths {
            if !llvm_bin_filepath.exists() {
                log::error!("Failed to find `{:?}`", llvm_bin_filepath);
                std::process::exit(1);
            }
        }

        Self {
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
        }
    }
}
