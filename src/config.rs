use core::panic;
use std::{
    env,
    path::{Path, PathBuf},
};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

use crate::{
    constants::DEFAULT_CONF_FILEPATH_UNDER_HOME,
    utils::{execute_llvm_config, find_llvm_config},
};

lazy_static! {
    pub static ref RLLVM_CONFIG: RLLVMConfig = RLLVMConfig::new();
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RLLVMConfig {
    /// The filepath of `llvm-config`
    llvm_config_filepath: PathBuf,

    /// The filepath of `clang`
    clang_filepath: PathBuf,

    /// The filepath of `clang++`
    clangxx_filepath: PathBuf,

    /// The filepath of `llvm-ar`
    llvm_ar_filepath: PathBuf,

    /// The filepath of `llvm-link`
    llvm_link_filepath: PathBuf,

    /// The filepath of `llvm-objcopy`
    llvm_objcopy_filepath: PathBuf,

    /// The path of the directory that stores intermediate bitcode files
    bitcode_store_path: Option<String>,

    /// Extra linking flags for link time optimization
    lto_ldflags: Option<Vec<String>>,

    /// Extra flags for bitcode generation, e.g., "-flto -fwhole-program-vtables"
    bitcode_generation_flags: Option<Vec<String>>,

    /// The configure only mode
    is_configure_only: bool,
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

    pub fn bitcode_store_path(&self) -> Option<&String> {
        self.bitcode_store_path.as_ref()
    }

    pub fn lto_ldflags(&self) -> Option<&Vec<String>> {
        self.lto_ldflags.as_ref()
    }

    pub fn bitcode_generation_flags(&self) -> Option<&Vec<String>> {
        self.bitcode_generation_flags.as_ref()
    }

    pub fn is_configure_only(&self) -> bool {
        self.is_configure_only
    }
}

impl RLLVMConfig {
    pub fn new() -> Self {
        let config_filepath = PathBuf::from(env::var("HOME").unwrap_or("".into()))
            .join(DEFAULT_CONF_FILEPATH_UNDER_HOME);
        Self::load_path(config_filepath)
    }

    pub fn load_path<P>(config_filepath: P) -> Self
    where
        P: AsRef<Path> + std::fmt::Debug,
    {
        let config_filepath = config_filepath.as_ref();
        match confy::load_path(config_filepath) {
            Ok(config) => config,
            Err(err) => {
                panic!(
                    "Failed to load configuration: config_filepath={:?}, err={}",
                    config_filepath, err
                )
            }
        }
    }
}

impl Default for RLLVMConfig {
    fn default() -> Self {
        log::info!("Infer rllvm configurations ...");

        // Find `llvm-config`
        let llvm_config_filepath = find_llvm_config()
            .unwrap_or_else(|err| panic!("Failed to find `llvm-config`: {:?}", err));
        log::info!("- llvm-config: {:?}", llvm_config_filepath);

        // Obtain LLVM version
        match execute_llvm_config(&llvm_config_filepath, &["--version"]) {
            Ok(llvm_version) => log::info!("- LLVM version: {}", llvm_version),
            Err(err) => log::warn!("- LLVM version: (unknown, err={:?})", err),
        }

        let llvm_bindir = execute_llvm_config(&llvm_config_filepath, &["--bindir"]).map_or_else(
            |err| panic!("Failed to execute `llvm-config --bindir`: {:?}", err),
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
            &llvm_ar_filepath,
            &llvm_link_filepath,
            &llvm_objcopy_filepath,
        ];
        for llvm_bin_filepath in llvm_bin_filepaths {
            if !llvm_bin_filepath.exists() {
                panic!("Failed to find `{:?}`", llvm_bin_filepath);
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
            lto_ldflags: None,
            bitcode_generation_flags: None,
            is_configure_only: false,
        }
    }
}
