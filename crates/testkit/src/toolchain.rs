//! Locating the configured LLVM, and pinning tests to an isolated config.
//!
//! Split from the fixtures so a test binary needing only the toolchain can
//! compile this file alone. Each binary compiles `common` separately, and an
//! item that binary does not reach is a `dead_code` error under `-D warnings`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// A tool from the configured LLVM's bindir.
pub fn llvm_bin(name: &str) -> PathBuf {
    let config = rllvm_core::utils::find_llvm_config().unwrap();
    let output = Command::new(config).arg("--bindir").output().unwrap();
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).unwrap().trim()).join(name)
}

/// Writes an `RLLVM_CONFIG` pointing at the configured toolchain. Tests must
/// never read or modify the developer's own configuration.
///
/// `llvm_objcopy_filepath` is part of that toolchain. Leaving it out sent every
/// caller down the `object`-crate rebuild instead, which is the fallback for
/// hosts without `llvm-objcopy` and does not model every target -- a RISC-V
/// cross build fails on it with `Unknown relocation target`. `rllvm-init`
/// records the key whenever the tool exists, so omitting it here tested a
/// configuration almost no user has, and the examples are exactly what is
/// supposed to prove the documented flow still works. The fallback keeps its
/// own coverage, from the one test that drops this key deliberately.
pub fn scratch_rllvm_config(directory: &Path) -> PathBuf {
    let contents = format!(
        "llvm_config_filepath = '{}'\n\
         clang_filepath = '{}'\n\
         clangxx_filepath = '{}'\n\
         llvm_objcopy_filepath = '{}'\n\
         llvm_ar_filepath = '{}'\n\
         llvm_link_filepath = '{}'\n",
        llvm_bin("llvm-config").display(),
        llvm_bin("clang").display(),
        llvm_bin("clang++").display(),
        llvm_bin("llvm-objcopy").display(),
        llvm_bin("llvm-ar").display(),
        llvm_bin("llvm-link").display(),
    );
    let path = directory.join("rllvm-config.toml");
    std::fs::write(&path, contents).unwrap();
    path
}
