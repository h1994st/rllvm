use clap::{CommandFactory, FromArgMatches};
use rllvm::{
    cli::{ClangWrapperArgs, verbose_log_level},
    compiler_wrapper::{
        CompilerKind, CompilerWrapper, CompilerWrapperBuilder, llvm::ClangWrapperBuilder,
    },
    config::try_rllvm_config,
    error::Error,
};
use tracing_subscriber::FmtSubscriber;

pub fn rllvm_main(name: &str, compiler_kind: CompilerKind) -> Result<(), Error> {
    // `rllvm-cxx` reuses this entry point, so the command name has to follow the
    // binary rather than be baked into the derive -- otherwise `rllvm-cxx
    // --rllvm-version` reports `rllvm-cc`, and its usage line is wrong too.
    let bin_name = match compiler_kind {
        CompilerKind::Clang => "rllvm-cc",
        CompilerKind::ClangXX => "rllvm-cxx",
    };
    let matches = ClangWrapperArgs::command().name(bin_name).get_matches();
    let args = ClangWrapperArgs::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());

    // Set log level. `--rllvm-verbose` overrides the configured level.
    let log_level = match verbose_log_level(args.common.verbose) {
        Some(level) => level,
        None => try_rllvm_config()?.log_level(),
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
