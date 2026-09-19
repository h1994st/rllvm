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
//! compilation database and the binaries.
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

/// Source-level queries over captured bitcode, linking LLVM directly.
#[cfg(feature = "query")]
pub mod query;

#[cfg(test)]
mod tests {
    use rllvm_core::config::{RLLVMConfig, pin_inferred_config};

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
    #[test]
    fn the_resolved_configuration_is_inferred_and_never_the_users_own() {
        let resolved = pin_inferred_config().expect("no usable LLVM configuration");
        let inferred = RLLVMConfig::try_default().expect("no usable LLVM configuration");
        assert_eq!(
            serde_json::to_value(resolved).unwrap(),
            serde_json::to_value(&inferred).unwrap(),
            "the process-wide configuration did not come from RLLVMConfig::try_default: \
             a test resolved it from ~/.rllvm/config.toml or $RLLVM_CONFIG before pinning"
        );
    }
}
