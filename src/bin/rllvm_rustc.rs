use std::{env, path::PathBuf};

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
    // When used as RUSTC_WRAPPER, cargo invokes: rllvm-rustc rustc <args...>
    // When used as RUSTC, cargo invokes: rllvm-rustc <args...>
    // We need to handle both cases.
    let mut raw_args: Vec<String> = env::args().collect();

    // Cargo never sends these, and it always passes either `rustc` or a rustc
    // flag first, so intercepting them in the leading position cannot shadow a
    // real argument. Without this, `rllvm-rustc --rllvm-version` reaches the
    // bitcode-path logic and answers with a complaint about `--out-dir`.
    if matches!(
        raw_args.get(1).map(String::as_str),
        Some("--rllvm-help" | "--rllvm-version")
    ) {
        // Both are clap actions: they print and exit.
        RustcWrapperArgs::parse();
    }

    // Honoured in the same position and removed before the split below, so
    // rustc never sees it. Under cargo nothing can pass it, which is what
    // `RLLVM_LOG_LEVEL` is for; by hand it works like it does on `rllvm-cc`.
    let verbose = take_leading_verbose(&mut raw_args);

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

    // `log_level` from the config, with `$RLLVM_LOG_LEVEL` over it, and
    // `--rllvm-verbose` over both.
    let log_level = verbose_log_level(verbose).unwrap_or_else(|| {
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

/// Take a leading `--rllvm-verbose[=LEVEL]` out of `args`, returning its level.
///
/// Only the leading position, and only the `=` form carries a value -- the same
/// `require_equals` rule `rllvm-cc` parses under, so a bare flag cannot swallow
/// the `rustc` path cargo puts next.
fn take_leading_verbose(args: &mut Vec<String>) -> u8 {
    let Some(arg) = args.get(1) else {
        return 0;
    };

    let level = if arg == "--rllvm-verbose" {
        1
    } else if let Some(value) = arg.strip_prefix("--rllvm-verbose=") {
        value.parse().unwrap_or(1)
    } else {
        return 0;
    };

    args.remove(1);
    level
}

#[cfg(test)]
mod tests {
    use super::take_leading_verbose;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn takes_the_leading_verbose_flag_and_removes_it() {
        let mut a = args(&[
            "rllvm-rustc",
            "--rllvm-verbose",
            "rustc",
            "--crate-name",
            "x",
        ]);
        assert_eq!(take_leading_verbose(&mut a), 1);
        assert_eq!(a, args(&["rllvm-rustc", "rustc", "--crate-name", "x"]));

        let mut a = args(&["rllvm-rustc", "--rllvm-verbose=3", "rustc"]);
        assert_eq!(take_leading_verbose(&mut a), 3);
        assert_eq!(a, args(&["rllvm-rustc", "rustc"]));
    }

    #[test]
    fn leaves_cargo_s_own_command_line_alone() {
        // What cargo actually invokes: nothing to take, nothing removed.
        let mut a = args(&["rllvm-rustc", "rustc", "--crate-name", "x"]);
        assert_eq!(take_leading_verbose(&mut a), 0);
        assert_eq!(a, args(&["rllvm-rustc", "rustc", "--crate-name", "x"]));

        // Not in the leading position: rustc's, not ours.
        let mut a = args(&["rllvm-rustc", "rustc", "--rllvm-verbose"]);
        assert_eq!(take_leading_verbose(&mut a), 0);
        assert_eq!(a.len(), 3);

        let mut a = args(&["rllvm-rustc"]);
        assert_eq!(take_leading_verbose(&mut a), 0);
    }
}
