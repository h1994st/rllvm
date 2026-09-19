//! Whole-program LLVM bitcode generation in Rust.
//!
//! `rllvm` provides compiler wrappers that build whole-program LLVM bitcode
//! alongside a normal build, and tools to extract and analyze it. It follows
//! the `CC`/`CXX` → build → extract workflow that
//! [wllvm](https://github.com/travitch/whole-program-llvm) and
//! [gllvm](https://github.com/SRI-CSL/gllvm) established.
//!
//! # Overview
//!
//! The compiler wrappers ([`compiler_wrapper`]) intercept `clang`/`clang++` invocations,
//! run the real compiler normally, then also generate LLVM bitcode and embed the bitcode
//! file path into a special section of the output object file. The extraction tool
//! (`rllvm-get-bc`) later reads those paths and links the bitcode together.
//!
//! Argument classification, bitcode capture, catalogs and configuration live in
//! [`rllvm_core`]; this crate holds the concrete clang and rustc drivers, the
//! compilation database and the binaries. Source-level queries over captured
//! bitcode live in `rllvm-query`, the only crate that links LLVM.
//!
//! # Configuration
//!
//! See [`rllvm_core::config`] for TOML-based configuration via
//! `~/.rllvm/config.toml`.

// Keeps the public surface deliberate: a `pub` item that no `pub use`
// re-exports is a mistake, not API.
#![warn(unreachable_pub)]

/// Command-line definitions shared by the binaries and the completion
/// generator. Not a supported interface.
#[doc(hidden)]
pub mod cli;

pub mod compilation_database;

/// Concrete clang and rustc compiler wrappers.
pub mod compiler_wrapper;

#[cfg(test)]
mod tests {
    use std::{sync::LazyLock, time::SystemTime};

    use rllvm_core::config::{RLLVMConfig, config_filepath, pin_inferred_config};

    /// The home configuration file's existence and, if present, modification
    /// time. `None` means the file does not exist.
    fn config_file_fingerprint() -> Option<SystemTime> {
        std::fs::metadata(config_filepath())
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    /// The configuration file's fingerprint at the earliest point this test
    /// can observe it -- before it calls `pin_inferred_config` below.
    ///
    /// This is a `LazyLock` rather than a plain call at the top of the test
    /// body to make the intent explicit: it is a "first touch" baseline, not
    /// a process-start one. It cannot be a process-start baseline, because
    /// nothing short of a pre-`main` hook (e.g. the `ctor` crate, not a
    /// dependency here) runs before `cargo test`'s harness starts dispatching
    /// `#[test]` fns, and this binary runs all of this crate's unit tests --
    /// including the ones in `compiler_wrapper` that also resolve the
    /// configuration -- in one process, on multiple threads, in an
    /// unspecified order. If a future test ever reached the unpinned
    /// production path before this test's body started, the file would
    /// already exist (or already be rewritten) by the time this baseline is
    /// taken, and no observation made from inside a `#[test]` fn can tell
    /// that apart from a file that predates the process. Closing that gap
    /// is not worth a new dependency for one guard, so this catches the case
    /// Finding 1 was written against -- this test's own call being the one
    /// that resolves the configuration for the first time in the process --
    /// and does not claim to catch a rogue test that wins that race first.
    static CONFIG_FILE_BEFORE: LazyLock<Option<SystemTime>> =
        LazyLock::new(config_file_fingerprint);

    /// Nothing in this binary may resolve the configuration from
    /// `~/.rllvm/config.toml` or `$RLLVM_CONFIG`.
    ///
    /// `rllvm-core` resolves the configuration once per process. Its own unit
    /// tests get an inferred one from a `cfg(test)` variant; this crate's do
    /// not, so each test that builds a wrapper calls `pin_inferred_config`
    /// first. Forgetting that is not a quiet read: outside a test,
    /// `RLLVMConfig::new` *writes* an inferred configuration to
    /// `~/.rllvm/config.toml` when none is there, so a test that forgets
    /// creates the developer's -- or a CI machine's -- home configuration.
    ///
    /// A grep cannot check this. The call sites that resolve the
    /// configuration are in `rllvm-core` and are reached transitively, so what
    /// has to hold is "no test in this binary reaches any of them without
    /// pinning first", which is a property of the call graph and of the order
    /// the harness happens to run in. This asserts the outcome instead: pin
    /// (a no-op once anything has resolved the configuration), then require
    /// that what got resolved is the inferred configuration. A test that ever
    /// wins the race against the user's own file fails here, loudly, rather
    /// than leaving the suite reading someone's machine.
    ///
    /// The value comparison alone is blind on a machine with no pre-existing
    /// `~/.rllvm/config.toml` -- every CI runner: an unpinned resolution
    /// there still infers the same values it would have pinned, so the
    /// comparison passes while the file is created as a side effect. The
    /// fingerprint check below closes that: it fails loudly if resolving the
    /// configuration created or modified the file, even when the resolved
    /// values are correct.
    #[test]
    fn the_resolved_configuration_is_inferred_and_never_the_users_own() {
        let before = *CONFIG_FILE_BEFORE;

        let resolved = pin_inferred_config().expect("no usable LLVM configuration");
        let inferred = RLLVMConfig::try_default().expect("no usable LLVM configuration");
        assert_eq!(
            serde_json::to_value(resolved).unwrap(),
            serde_json::to_value(&inferred).unwrap(),
            "the process-wide configuration did not come from RLLVMConfig::try_default: \
             a test resolved it from ~/.rllvm/config.toml or $RLLVM_CONFIG before pinning"
        );

        assert_eq!(
            before,
            config_file_fingerprint(),
            "{:?} was created or modified while resolving the configuration: \
             a test reached RLLVMConfig::new (or ::load_path) instead of pin_inferred_config",
            config_filepath()
        );
    }
}
