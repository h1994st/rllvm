use std::{env, path::PathBuf};

use rllvm::{compiler_wrapper::llvm::RustcWrapper, config::try_rllvm_config, error::Error};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

fn main() -> Result<(), Error> {
    // When used as RUSTC_WRAPPER, cargo invokes: rllvm-rustc rustc <args...>
    // When used as RUSTC, cargo invokes: rllvm-rustc <args...>
    // We need to handle both cases.
    let raw_args: Vec<String> = env::args().collect();

    // Detect RUSTC_WRAPPER mode: if the second argument is a path to rustc
    // (doesn't start with '-' and contains "rustc"), treat it as the rustc path.
    let (rustc_path, rustc_args) = if raw_args.len() > 1
        && !raw_args[1].starts_with('-')
        && (raw_args[1].ends_with("rustc") || raw_args[1].contains("/rustc"))
    {
        // RUSTC_WRAPPER mode: argv[1] is the real rustc path
        (PathBuf::from(&raw_args[1]), raw_args[2..].to_vec())
    } else {
        // RUSTC mode: `rustc_filepath` in the config, `$RLLVM_REAL_RUSTC` over
        // that, and `PATH` when neither is set. Tolerates a missing config,
        // because the wrapper still has useful work to do without one.
        let configured = try_rllvm_config()
            .ok()
            .and_then(|config| config.rustc_filepath());
        let rustc = configured
            .unwrap_or_else(|| which::which("rustc").unwrap_or_else(|_| PathBuf::from("rustc")));
        (rustc, raw_args[1..].to_vec())
    };

    // `log_level` from the config, with `$RLLVM_LOG_LEVEL` over it. Cargo owns
    // this command line, so there is no `--rllvm-verbose` to offer here.
    let log_level = try_rllvm_config()
        .map(|config| config.log_level())
        .unwrap_or(Level::ERROR);
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
