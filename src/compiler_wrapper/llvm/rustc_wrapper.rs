//! Rustc compiler wrapper for LLVM bitcode extraction
//!
//! Wraps `rustc` to transparently generate LLVM bitcode alongside normal
//! compilation. Users set `RUSTC=rllvm-rustc` (or `RUSTC_WRAPPER=rllvm-rustc`)
//! so that Cargo invokes this wrapper instead of `rustc` directly.
//!
//! rustc offers two places to attach the bitcode path and no third. A crate
//! type that links takes a marker object through `-C link-arg`; a crate type
//! that archives is patched afterwards, member by member. Both carry the same
//! crate-level path, so whichever object the linker ends up pulling in brings
//! the whole crate's bitcode with it.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::{marker, rustc_args, rustc_marker};
use crate::{
    compiler_wrapper::CompilerKind, config::try_rllvm_config, error::Error,
    utils::embed_bitcode_filepath_to_object_file,
};

/// Rustc wrapper that generates LLVM bitcode alongside normal compilation.
#[derive(Debug)]
pub struct RustcWrapper {
    /// Path to the real `rustc` binary
    rustc_path: PathBuf,
    /// Whether to suppress diagnostic output
    is_silent: bool,
}

impl RustcWrapper {
    /// Create a new `RustcWrapper` that delegates to the given `rustc` binary.
    pub fn new(rustc_path: PathBuf) -> Self {
        Self {
            rustc_path,
            is_silent: false,
        }
    }

    pub fn silence(&mut self, value: bool) -> &mut Self {
        self.is_silent = value;
        self
    }

    /// Run rustc once, emitting bitcode alongside the normal output and
    /// recording its path where `rllvm-get-bc` will find it.
    ///
    /// Bitcode comes from adding `llvm-bc=<path>` to `--emit`, so the crate is
    /// compiled once rather than twice.
    pub fn run<S>(&self, args: &[S]) -> Result<Option<i32>, Error>
    where
        S: AsRef<OsStr> + AsRef<str> + std::fmt::Debug,
    {
        let args: Vec<&str> = args.iter().map(|a| <S as AsRef<str>>::as_ref(a)).collect();

        let Some(actions) = rustc_args::classify(&args) else {
            return self.spawn(&args);
        };

        let store = try_rllvm_config()?.bitcode_store_path().cloned();
        let bitcode = rustc_args::bitcode_path(&args, store.as_deref())?;

        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let mut rewritten = rustc_args::rewrite_emit(&owned, &bitcode);

        // Bound to a variable rather than dropped at the end of the branch:
        // the marker has to still exist when rustc invokes the linker.
        let marker_dir = if actions.links {
            Some(tempfile::tempdir()?)
        } else {
            None
        };
        if let Some(dir) = &marker_dir {
            // rustc drives its own linker, so there are no C compile arguments
            // to inherit: the configured `clang` for the host is what this
            // path has always used.
            let clang = try_rllvm_config()?.clang_filepath();
            let marker =
                marker::build_marker_object(&bitcode, dir.path(), clang, CompilerKind::Clang, &[])?;
            rewritten.push("-C".to_string());
            rewritten.push(format!("link-arg={}", marker.display()));
        }

        if !self.is_silent {
            tracing::debug!("rustc: bitcode={bitcode:?}, actions={actions:?}");
        }

        let code = self.spawn(&rewritten)?;
        if code != Some(0) {
            return Ok(code);
        }

        if actions.archives || actions.object {
            self.embed_into_outputs(&args, &bitcode)?;
        }

        Ok(Some(0))
    }

    /// Run rustc with the given arguments and hand back its exit code.
    fn spawn<S>(&self, args: &[S]) -> Result<Option<i32>, Error>
    where
        S: AsRef<OsStr>,
    {
        let status = Command::new(&self.rustc_path).args(args).status()?;
        Ok(status.code())
    }

    /// Record the bitcode path in whatever rustc just wrote.
    ///
    /// Dispatches on what the artifact turns out to be rather than on the
    /// crate type: `--emit=obj` and `--emit=link` can both come from
    /// `--crate-type=lib`, and only the file says which happened.
    fn embed_into_outputs(&self, args: &[&str], bitcode: &Path) -> Result<(), Error> {
        for artifact in self.output_artifacts(args)? {
            if !artifact.exists() {
                continue;
            }

            let data = fs::read(&artifact)?;
            if object::read::archive::ArchiveFile::parse(&*data).is_ok() {
                let patched = rustc_marker::patch_archive(&artifact, bitcode)?;
                tracing::debug!("rustc: patched {patched} members of {artifact:?}");
            } else if object::File::parse(&*data).is_ok() {
                embed_bitcode_filepath_to_object_file::<&Path>(bitcode, &artifact, None)?;
                tracing::debug!("rustc: embedded the bitcode path into {artifact:?}");
            } else {
                tracing::debug!("rustc: {artifact:?} is neither an archive nor an object");
            }
        }

        Ok(())
    }

    /// The files rustc writes for this invocation.
    ///
    /// `-o` names the single output outright. Otherwise rustc is asked, rather
    /// than rllvm reimplementing its naming rules across crate types and
    /// platforms; `--print` does not compile, so this costs one spawn.
    fn output_artifacts(&self, args: &[&str]) -> Result<Vec<PathBuf>, Error> {
        if let Some(output) = rustc_args::flag_value(args, "-o") {
            return Ok(vec![PathBuf::from(output)]);
        }

        let output = Command::new(&self.rustc_path)
            .args(args)
            .arg("--print=file-names")
            .output()?;
        if !output.status.success() {
            return Err(Error::ExecutionFailure(format!(
                "rustc --print=file-names failed, so the output to record the bitcode path in is unknown: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let out_dir = rustc_args::flag_value(args, "--out-dir").unwrap_or(".");
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| PathBuf::from(out_dir).join(name))
            .collect())
    }
}
