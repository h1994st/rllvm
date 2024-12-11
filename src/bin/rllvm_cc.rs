use std::env;

use rllvm::{
    compiler_wrapper::{llvm::ClangWrapper, CompilerKind, CompilerWrapper},
    config::rllvm_config,
    error::Error,
};
use simple_logger::SimpleLogger;

pub fn rllvm_main(name: &str, compiler_kind: CompilerKind) -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    // Skip the first argument
    let args = &args[1..];

    // Set log level
    let log_level = rllvm_config().log_level().to_level_filter();
    SimpleLogger::new()
        .with_level(log_level)
        .init()
        .map_err(|err| Error::LoggerError(err.to_string()))?;

    let mut cc = ClangWrapper::new(name, compiler_kind);

    if let Some(code) = cc.parse_args(&args)?.run()? {
        std::process::exit(code);
    }

    Ok(())
}

pub fn main() -> Result<(), Error> {
    rllvm_main("rllvm", CompilerKind::Clang)
}
