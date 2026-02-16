use std::{env, path::PathBuf};

use rllvm::{compiler_wrapper::llvm::RustcWrapper, error::Error};
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
        // RUSTC mode: find rustc ourselves
        let rustc = env::var("RLLVM_REAL_RUSTC")
            .map(PathBuf::from)
            .unwrap_or_else(|_| which::which("rustc").unwrap_or_else(|_| PathBuf::from("rustc")));
        (rustc, raw_args[1..].to_vec())
    };

    // Set up logging from RLLVM_LOG_LEVEL env var
    let log_level = match env::var("RLLVM_LOG_LEVEL")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0)
    {
        0 => Level::ERROR,
        1 => Level::WARN,
        2 => Level::INFO,
        3 => Level::DEBUG,
        _ => Level::TRACE,
    };
    let _ = FmtSubscriber::builder()
        .with_max_level(log_level)
        .try_init();

    tracing::debug!(
        "rllvm-rustc: rustc_path={:?}, args={:?}",
        rustc_path,
        rustc_args
    );

    let wrapper = RustcWrapper::new(rustc_path);
    if let Some(code) = wrapper.run(&rustc_args)? {
        if code != 0 {
            std::process::exit(code);
        }
    }

    Ok(())
}
