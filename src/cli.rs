//! Command-line definitions for the rllvm binaries.
//!
//! These live in the library, rather than privately inside each binary, so
//! `rllvm-completions` can generate from the same definitions the binaries
//! parse with. A hand-written second copy is how the completions came to offer
//! `-c` and `--compiler` for years after those flags were removed.
//!
//! Not a supported interface: it is `pub` only so the binaries in this crate
//! can share it, and it changes whenever their arguments change.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::merge::MergeStrategy;

/// The log level `--rllvm-verbose` asks for.
///
/// `None` means the flag was not given, so the configured `log_level` stands.
/// Shared so the wrappers cannot map the same number to different levels.
pub fn verbose_log_level(verbose: u8) -> Option<tracing::Level> {
    match verbose {
        0 => None,
        1 => Some(tracing::Level::WARN),
        2 => Some(tracing::Level::INFO),
        3 => Some(tracing::Level::DEBUG),
        _ => Some(tracing::Level::TRACE),
    }
}

/// The options every rllvm wrapper answers, whatever it wraps.
///
/// Flattened into each wrapper rather than repeated in it. These two are the
/// wrapper's entire user-facing surface on a command line the compiler
/// otherwise owns, so they have to agree across wrappers -- and two copies of
/// an argument definition drift, which is exactly how the completions came to
/// describe flags that had been removed.
///
/// `--rllvm-compiler` is not here: it names a *clang* to wrap, and the rustc
/// wrapper takes its compiler from `RLLVM_REAL_RUSTC` because cargo owns a
/// `RUSTC_WRAPPER`'s command line. Every wrapper honours the rest, including
/// when invoked by hand -- `RLLVM_LOG_LEVEL` remains the route under cargo,
/// which cannot pass a flag.
#[derive(clap::Args, Debug)]
pub struct WrapperOptions {
    /// Verbose mode: `--rllvm-verbose` for level 1, `--rllvm-verbose=3` for level 3
    ///
    /// A repeated-count flag would mean writing `--rllvm-verbose` three times,
    /// since there is no short form to spare — `-v` belongs to the compiler.
    /// `require_equals` keeps the value from swallowing the next compiler
    /// argument, so `--rllvm-verbose -c foo.c` parses as level 1 plus `-c foo.c`.
    #[arg(
        long = "rllvm-verbose",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "1",
        default_value = "0"
    )]
    pub verbose: u8,

    /// Print this help (the wrapped compiler owns plain `--help`)
    #[arg(long = "rllvm-help", action = clap::ArgAction::Help)]
    pub rllvm_help: Option<bool>,

    /// Print the wrapper version (the wrapped compiler owns plain `--version`)
    #[arg(long = "rllvm-version", action = clap::ArgAction::Version)]
    pub rllvm_version: Option<bool>,
}

// Wrapper arguments.
//
// This must be usable as a drop-in `CC`, so every argument the compiler could
// plausibly own has to reach the compiler:
//
// - Compiler arguments are collected as a trailing var-arg, so no `--`
//   separator is needed. `--` still works, for callers that want to be explicit.
// - The wrapper's own options are long-only and prefixed `--rllvm-`. Clang has
//   no `--rllvm-*` flags, so collision is impossible. In particular `-c` and
//   `-v` belong to the compiler, not to us.
// - clap's built-in `--help`/`--version` are disabled and re-exposed under the
//   prefix. Build systems identify the compiler by running `$CC --version`; if
//   the wrapper answered that, CMake and autoconf would misidentify the
//   toolchain, which looks nothing like an argument-parsing bug.
//
// These notes are deliberately NOT doc comments: clap promotes a struct's doc
// comment to `long_about`, which would print this rationale to anyone running
// `--rllvm-help`.
#[derive(Parser, Debug)]
#[command(
    about = "Execute the wrapped clang compiler",
    long_about = None,
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct ClangWrapperArgs {
    /// Path to the wrapped compiler
    #[arg(long = "rllvm-compiler")]
    pub compiler: Option<PathBuf>,

    #[command(flatten)]
    pub common: WrapperOptions,

    /// Compiler arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub clang_args: Vec<String>,
}

/// Extraction arguments
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-get-bc",
    about = "Extract a single bitcode file for the given input",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
pub struct ExtractionArgs {
    /// Input filepath for bitcode extraction
    pub input: PathBuf,

    /// Output filepath of the extracted bitcode file
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Build bitcode archive (only used for archive files, e.g., *.a).
    /// Equivalent to --merge-strategy=archive. Deprecated in favor of --merge-strategy.
    #[arg(short = 'b', long)]
    pub build_bitcode_archive: bool,

    /// Bitcode merge strategy: full (llvm-link all), partial (group by dir then link), archive (llvm-ar)
    #[arg(long, value_enum)]
    pub merge_strategy: Option<MergeStrategy>,

    /// Save manifest of all filepaths of underlying bitcode files
    #[arg(short = 'm', long)]
    pub save_manifest: bool,

    /// Directory that relative embedded bitcode paths resolve against
    ///
    /// Objects built with `RLLVM_BITCODE_ROOT` set record paths relative to that
    /// root, so they survive the build tree being moved, copied out of a
    /// container, or replayed from a compiler cache. Point this at wherever the
    /// tree lives now. Absolute entries, which is what older objects contain,
    /// are unaffected. Defaults to the current directory.
    #[arg(long)]
    pub bitcode_root: Option<PathBuf>,

    /// Verbose mode
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// CLI arguments for rllvm-init
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-init",
    about = "Auto-detect LLVM installation and generate rllvm configuration",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
pub struct InitArgs {
    /// Output path for the generated config file
    ///
    /// Defaults to wherever the rest of the toolchain reads its configuration
    /// from: `$RLLVM_CONFIG` when set, otherwise `~/.rllvm/config.toml`.
    #[arg(short = 'o', long)]
    pub output: Option<String>,

    /// Print detected configuration without writing to disk
    #[arg(long)]
    pub dry_run: bool,

    /// Override LLVM installation path (directory containing bin/llvm-config)
    #[arg(long)]
    pub llvm_prefix: Option<PathBuf>,
}

/// Analyze LLVM bitcode files
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-info",
    about = "Display information about LLVM bitcode files",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
pub struct InfoArgs {
    /// Input file (bitcode .bc or object file with embedded bitcode)
    pub input: PathBuf,

    /// List all function names
    #[arg(short = 'f', long)]
    pub functions: bool,
}

/// The wrapper options `rllvm-rustc` answers.
///
/// Cargo owns a `RUSTC_WRAPPER`'s command line, so the compiler-selection and
/// verbosity overrides are environment variables there -- `RLLVM_REAL_RUSTC`
/// and `RLLVM_LOG_LEVEL` -- rather than flags nothing could pass. What is left
/// are [`WrapperOptions`] -- the informational flags a person runs by hand,
/// which cargo never sends, so answering them cannot disturb a build.
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-rustc",
    about = "Execute the wrapped rustc compiler",
    long_about = None,
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct RustcWrapperArgs {
    #[command(flatten)]
    pub common: WrapperOptions,

    /// rustc arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rustc_args: Vec<String>,
}

/// Which binary to generate completions for.
///
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum BinName {
    Cc,
    Cxx,
    GetBc,
    Init,
    Info,
    Rustc,
    Completions,
}

/// Generate shell completions for rllvm tools
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-completions",
    about = "Generate shell completions for rllvm tools",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
pub struct CompletionArgs {
    /// Shell to generate completions for
    #[arg(long, value_enum)]
    pub shell: clap_complete::Shell,

    /// Binary to generate completions for
    #[arg(long, value_enum, default_value = "cc")]
    pub bin: BinName,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The invariant that makes the wrappers safe as a drop-in `CC`, and the
    /// one the hand-written completions violated for several releases by
    /// offering `-c` and `--compiler`.
    #[test]
    fn every_wrapper_option_is_long_and_rllvm_prefixed() {
        for command in [ClangWrapperArgs::command(), RustcWrapperArgs::command()] {
            for arg in command.get_arguments() {
                assert!(
                    arg.get_short().is_none(),
                    "`-{}` would be taken from the compiler: {arg:?}",
                    arg.get_short().unwrap()
                );
                if let Some(long) = arg.get_long() {
                    assert!(
                        long.starts_with("rllvm-"),
                        "`--{long}` collides with the compiler's own options"
                    );
                }
            }
        }
    }

    /// `--help` and `--version` belong to the compiler: a build system runs
    /// `$CC --version` to identify the toolchain.
    #[test]
    fn the_wrapper_claims_neither_help_nor_version() {
        let command = ClangWrapperArgs::command();
        for reserved in ["help", "version"] {
            assert!(
                command
                    .get_arguments()
                    .all(|arg| arg.get_long() != Some(reserved)),
                "`--{reserved}` must reach the compiler"
            );
        }
    }

    /// Named flags rather than a count, so dropping one from the struct fails
    /// here rather than silently disappearing from the completions.
    #[test]
    fn extraction_exposes_every_documented_option() {
        let command = ExtractionArgs::command();
        let longs: Vec<_> = command
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        for expected in [
            "output",
            "build-bitcode-archive",
            "merge-strategy",
            "save-manifest",
            "bitcode-root",
            "verbose",
        ] {
            assert!(
                longs.contains(&expected),
                "--{expected} missing from {longs:?}"
            );
        }
    }
}
