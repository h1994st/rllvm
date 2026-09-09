use std::path::PathBuf;

use clap::Parser;
use rllvm::{
    cli::{RustcWrapperArgs, verbose_log_level},
    compiler_wrapper::llvm::RustcWrapper,
    config::try_rllvm_config,
    error::Error,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

fn main() -> Result<(), Error> {
    // Parsed exactly as `rllvm-cc` parses its own: the wrapper's `--rllvm-`
    // options are named, and everything else -- cargo's `rustc` path included
    // -- lands in a trailing var-arg that allows hyphen values. That keeps the
    // wrapper's options out of what rustc receives without any hand parsing,
    // and answers `--rllvm-help`/`--rllvm-version` on the way through.
    let args = RustcWrapperArgs::parse();
    let raw_args = args.rustc_args;

    // Detect RUSTC_WRAPPER mode: if the first argument is a path to rustc
    // (doesn't start with '-' and contains "rustc"), treat it as the rustc path.
    let (rustc_path, rustc_args) = if let Some(first) = raw_args.first()
        && !first.starts_with('-')
        && (first.ends_with("rustc") || first.contains("/rustc"))
    {
        // RUSTC_WRAPPER mode: the first argument is the real rustc path
        (PathBuf::from(first), raw_args[1..].to_vec())
    } else {
        // RUSTC mode: `rustc_filepath` in the config, `$RLLVM_REAL_RUSTC` over
        // that, and `PATH` when neither is set. Tolerates a missing config,
        // because the wrapper still has useful work to do without one.
        let configured = try_rllvm_config()
            .ok()
            .and_then(|config| config.rustc_filepath());
        let rustc = configured
            .unwrap_or_else(|| which::which("rustc").unwrap_or_else(|_| PathBuf::from("rustc")));
        (rustc, raw_args.clone())
    };

    // `log_level` from the config, with `$RLLVM_LOG_LEVEL` over it, and
    // `--rllvm-verbose` over both.
    let log_level = verbose_log_level(args.common.verbose).unwrap_or_else(|| {
        try_rllvm_config()
            .map(|config| config.log_level())
            .unwrap_or(Level::ERROR)
    });
    // Diagnostics belong on stderr; stdout is the wrapped compiler's output.
    let _ = FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_writer(std::io::stderr)
        .try_init();

    tracing::debug!(
        "rllvm-rustc: rustc_path={:?}, args={:?}",
        rustc_path,
        rustc_args
    );

    let wrapper = RustcWrapper::new(rustc_path);
    if let Some(code) = wrapper.run(&rustc_args)?
        && code != 0
    {
        std::process::exit(code);
    }

    Ok(())
}
