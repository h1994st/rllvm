use std::path::PathBuf;

use clap::Parser;
use rllvm::{
    compiler_wrapper::{
        CompilerKind, CompilerWrapper, CompilerWrapperBuilder, llvm::ClangWrapperBuilder,
    },
    config::try_rllvm_config,
    error::Error,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

/// Wrapper arguments.
///
/// This must be usable as a drop-in `CC`, so every argument the compiler could
/// plausibly own has to reach the compiler:
///
/// - Compiler arguments are collected as a trailing var-arg, so no `--`
///   separator is needed. `--` still works, for callers that want to be explicit.
/// - The wrapper's own options are long-only and prefixed `--rllvm-`. Clang has
///   no `--rllvm-*` flags, so collision is impossible. In particular `-c` and
///   `-v` belong to the compiler, not to us.
/// - clap's built-in `--help`/`--version` are disabled and re-exposed under the
///   prefix. Build systems identify the compiler by running `$CC --version`; if
///   the wrapper answered that, CMake and autoconf would misidentify the
///   toolchain, which looks nothing like an argument-parsing bug.
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-cc",
    about = "Execute the wrapped clang compiler",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct ClangWrapperArgs {
    /// Path to the wrapped compiler
    #[arg(long = "rllvm-compiler")]
    compiler: Option<PathBuf>,

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
    verbose: u8,

    /// Print this help (the compiler owns plain `--help`)
    #[arg(long = "rllvm-help", action = clap::ArgAction::Help)]
    rllvm_help: Option<bool>,

    /// Print the wrapper version (the compiler owns plain `--version`)
    #[arg(long = "rllvm-version", action = clap::ArgAction::Version)]
    rllvm_version: Option<bool>,

    /// Compiler arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    clang_args: Vec<String>,
}

pub fn rllvm_main(name: &str, compiler_kind: CompilerKind) -> Result<(), Error> {
    let args = ClangWrapperArgs::parse();

    // Set log level
    // The verbose flag will override the configured log level
    let log_level = if args.verbose == 0 {
        try_rllvm_config()?.log_level()
    } else {
        match args.verbose {
            1 => Level::WARN,
            2 => Level::INFO,
            3 => Level::DEBUG,
            _ => Level::TRACE,
        }
    };
    // Diagnostics belong on stderr: these wrappers stand in for a compiler, and
    // anything on stdout is captured as build output (`-E` preprocessing,
    // `-print-*` queries), where a log line corrupts the result.
    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_writer(std::io::stderr)
        .init();

    let mut cc_builder = ClangWrapperBuilder::new()
        .name(name)
        .compiler_kind(compiler_kind);
    if let Some(compiler) = args.compiler {
        cc_builder = cc_builder.wrapped_compiler(compiler);
    }
    let mut cc = cc_builder.build()?;

    if let Some(code) = cc.parse_args(&args.clang_args)?.run()? {
        std::process::exit(code);
    }

    Ok(())
}

pub fn main() -> Result<(), Error> {
    rllvm_main("rllvm", CompilerKind::Clang)
}
