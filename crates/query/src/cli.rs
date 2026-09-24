//! Command-line definitions for `rllvm-query`.
//!
//! These live in the library, rather than privately inside the binary, so the
//! binary and its completion script generate from one definition. A
//! hand-written second copy is how the wrapper completions came to offer
//! flags that had been removed.
//!
//! Not a supported interface: it is `pub` only so the binary in this crate
//! can use it, and it changes whenever the arguments change.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Arguments for `rllvm-query`.
///
/// The subcommand is optional so `rllvm-query --llvm-version` keeps working
/// with no query requested.
#[derive(Debug, Parser)]
#[command(
    name = "rllvm-query",
    about = "Query captured bitcode at source level",
    version
)]
pub struct QueryArgs {
    /// Print the LLVM version this binary links and exit
    #[arg(long = "llvm-version")]
    pub llvm_version: bool,

    /// Catalog JSON to query, e.g. from `rllvm-get-bc` or `rllvm-compdb generate`
    #[arg(long)]
    pub catalog: Option<PathBuf>,

    /// Include the heuristic address-taken inventory in `indirect-targets` answers
    ///
    /// Global so it may trail its subcommand -- `indirect-targets t.c:8
    /// --heuristics` -- which is how the README writes it and the only
    /// placement that reads naturally for a flag that modifies one query.
    #[arg(long, global = true)]
    pub heuristics: bool,

    /// Print the raw JSON answer envelope instead of text
    ///
    /// Global, like `--heuristics`, so it may trail the subcommand.
    #[arg(long, global = true, conflicts_with = "full")]
    pub json: bool,

    /// Also print scope, analysis, uncertainty and provenance, still as text
    #[arg(long, global = true)]
    pub full: bool,

    #[command(subcommand)]
    pub command: Option<QueryCommand>,
}

/// Direction for the `closure` query.
///
/// Mirrors [`crate::Direction`] field-for-field, but is not that type. It was
/// a separate type because `cli.rs` had to build without the `query` feature;
/// now that this module sits in the crate that defines `Direction`, nothing
/// forces the mirror, and collapsing the two is tracked for the
/// trait-narrowing change. The binary converts between them meanwhile.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ClosureDirection {
    /// Functions that reach the named function.
    In,
    /// Functions the named function reaches.
    Out,
}

/// The ten source-level queries `rllvm-query` answers, plus `Mcp` to serve
/// them over MCP stdio instead of running one and exiting, and `Completions`
/// to print a shell completion script -- twelve variants in all.
///
/// The ten query variants mirror [`crate::Query`] field-for-field, for the
/// same reason [`ClosureDirection`] mirrors [`crate::Direction`]. The binary
/// converts a parsed variant into a [`crate::Query`] before running it;
/// `Mcp` and `Completions` have no counterpart there -- they name a mode,
/// and `to_query` answers `None` for both.
///
/// This mirror can drift: `to_query` in `rllvm_query.rs` is exhaustive over
/// this type, so a variant added here without an arm there fails to compile,
/// but a variant added to [`crate::Query`] with no matching variant here
/// compiles cleanly on its own. `rllvm_query.rs`'s tests close that gap with
/// a reverse match, exhaustive over [`crate::Query`], so that drift also
/// fails to compile.
#[derive(clap::Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
pub enum QueryCommand {
    /// Every definition of the symbol, with module and configuration.
    Defs {
        /// Mangled symbol, full demangled reading, or a bare identifier
        name: String,
    },
    /// Functions with at least one instruction mapped to `file:line`, and
    /// the call sites recorded there.
    At {
        /// Source file, as recorded in debug info
        file: String,
        /// One-based source line
        line: u32,
    },
    /// Functions containing a call to the target, each with its call sites.
    Callers {
        /// Mangled symbol, full demangled reading, or a bare identifier
        name: String,
    },
    /// Outgoing call sites of the target, classified.
    Callees {
        /// Mangled symbol, full demangled reading, or a bare identifier
        name: String,
    },
    /// Non-call uses: how and where the function's address is taken.
    Uses {
        /// Mangled symbol, full demangled reading, or a bare identifier
        name: String,
    },
    /// One supporting path from `from` to `to`, or its explicit absence.
    Reach {
        /// Symbol to start from
        from: String,
        /// Symbol to reach
        to: String,
    },
    /// The set that can reach the target, or that it can reach.
    Closure {
        /// Mangled symbol, full demangled reading, or a bare identifier
        name: String,
        /// `in` for functions that reach it, `out` for functions it reaches
        #[arg(value_enum)]
        direction: ClosureDirection,
    },
    /// Unbound symbols: the captured program's boundary.
    Externals,
    /// Rust definitions exported under an unmangled name, callable from C.
    FfiExports,
    /// `!callees` at a call site, when CVP produced it; otherwise unresolved.
    IndirectTargets {
        /// `file:line` location, e.g. `t.c:4`
        at: String,
    },
    /// Serve the ten queries over MCP stdio: JSON-RPC 2.0, newline-delimited.
    Mcp,
    /// Print a shell completion script for `rllvm-query`
    ///
    /// `rllvm-completions` lives in the wrapper crate and cannot reach
    /// [`QueryArgs`] without making every wrapper build link LLVM, so this
    /// binary generates its own.
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}
