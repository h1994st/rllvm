use std::path::PathBuf;

use clap::Parser;
use rllvm::{
    compiler_wrapper::{
        CompilerKind, CompilerWrapper, CompilerWrapperBuilder, llvm::ClangWrapperBuilder,
    },
    config::rllvm_config,
    error::Error,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

/// Extraction arguments
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-cc",
    about = "Execute the wrapped clang compiler",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
struct ClangWrapperArgs {
    /// Path to the wrapped compiler
    #[arg(short = 'c', long)]
    compiler: Option<PathBuf>,

    /// Verbose mode
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Compiler arguments
    #[arg(last = true)]
    clang_args: Vec<String>,
}

pub fn rllvm_main(name: &str, compiler_kind: CompilerKind) -> Result<(), Error> {
    let args = ClangWrapperArgs::parse();

    // Set log level
    // The verbose flag will override the configured log level
    let log_level = if args.verbose == 0 {
        rllvm_config().log_level()
    } else {
        match args.verbose {
            1 => Level::WARN,
            2 => Level::INFO,
            3 => Level::DEBUG,
            _ => Level::TRACE,
        }
    };
    FmtSubscriber::builder().with_max_level(log_level).init();

    let mut cc_builder = ClangWrapperBuilder::new()
        .name(name)
        .compiler_kind(compiler_kind);
    if let Some(compiler) = args.compiler {
        cc_builder = cc_builder.wrapped_compiler(compiler);
    }
    let mut cc = cc_builder.build();

    if let Some(code) = cc.parse_args(&args.clang_args)?.run()? {
        std::process::exit(code);
    }

    Ok(())
}

pub fn main() -> Result<(), Error> {
    rllvm_main("rllvm", CompilerKind::Clang)
}
