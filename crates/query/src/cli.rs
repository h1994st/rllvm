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

use clap::{Args, Parser, ValueEnum};

use crate::Confidence;

/// Arguments for `rllvm-query`.
///
/// The subcommand is optional so `rllvm-query --llvm-version` keeps working
/// with no query requested, and so `--catalog` alone can answer the queries
/// piped on stdin.
#[derive(Debug, Parser)]
#[command(
    name = "rllvm-query",
    about = "Query captured bitcode at source level",
    after_help = "With --catalog and no query, reads one query per line from stdin, \
                  written as on the command line, and answers them all from one load \
                  of the catalog.",
    version
)]
pub struct QueryArgs {
    /// Print the LLVM version this binary links and exit
    #[arg(long = "llvm-version")]
    pub llvm_version: bool,

    /// Catalog JSON to query, e.g. from `rllvm-get-bc` or `rllvm-compdb generate`
    #[arg(long)]
    pub catalog: Option<PathBuf>,

    #[command(flatten)]
    pub modifiers: QueryModifiers,

    /// Print the raw JSON answer envelope instead of text
    ///
    /// Global, like `--heuristics`, so it may trail the subcommand.
    #[arg(long, global = true, conflicts_with = "full")]
    pub json: bool,

    /// Also print scope, analysis, uncertainty and provenance, still as text
    #[arg(long, global = true)]
    pub full: bool,

    /// Overlay file of agent-authored call-graph edges [default: `<catalog stem>.overlay.jsonl`]
    ///
    /// Global, like `--json`, so it may trail the `overlay` subcommand.
    #[arg(long, global = true, value_name = "PATH")]
    pub overlay: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<QueryCommand>,
}

/// The flags that modify a query rather than name one. Shared by the command
/// line and each line of stdin, which may carry them too.
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct QueryModifiers {
    /// Include the heuristic address-taken inventory in `indirect-targets` answers
    ///
    /// Global so it may trail its subcommand -- `indirect-targets t.c:8
    /// --heuristics` -- which is how the README writes it and the only
    /// placement that reads naturally for a flag that modifies one query.
    #[arg(long, global = true)]
    pub heuristics: bool,

    /// Also walk agent-authored overlay edges in `reach` and `closure`; never proof
    ///
    /// Global, like `--heuristics`. Every step through an overlay edge is
    /// labeled `agent`, and an answer that used one says it is not proven.
    #[arg(long, global = true)]
    pub include_overlay: bool,

    /// The weakest overlay edge `--include-overlay` walks [default: low]
    #[arg(long, global = true, value_enum, value_name = "LEVEL")]
    pub min_confidence: Option<Confidence>,
}

impl QueryModifiers {
    /// These flags with `line`'s added: a flag set in either is set.
    pub fn with(self, line: QueryModifiers) -> QueryModifiers {
        QueryModifiers {
            heuristics: self.heuristics || line.heuristics,
            include_overlay: self.include_overlay || line.include_overlay,
            min_confidence: line.min_confidence.or(self.min_confidence),
        }
    }
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

/// The eleven source-level queries `rllvm-query` answers, plus `Mcp` to serve
/// them over MCP stdio instead of running one and exiting, `Completions` to
/// print a shell completion script, `Cache` to inspect or prune the
/// per-module facts cache, and `Overlay` to keep agent-authored call-graph
/// edges.
///
/// The eleven query variants mirror [`crate::Query`] field-for-field, for the
/// same reason [`ClosureDirection`] mirrors [`crate::Direction`]. The binary
/// converts a parsed variant into a [`crate::Query`] before running it;
/// `Mcp`, `Completions`, `Cache` and `Overlay` have no counterpart there --
/// they name a mode, and `to_query` answers `None` for each.
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
    /// Unresolved indirect call sites grouped by the record field they dispatch through, with the functions stored into that field. Candidates, not edges.
    ResolutionCandidates,
    /// Serve the eleven queries over MCP stdio: JSON-RPC 2.0, newline-delimited.
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
    /// Show the per-module facts cache's location and disk use, or prune it.
    Cache {
        #[command(subcommand)]
        action: Option<CacheAction>,
    },
    /// Record, list or compact agent-authored call-graph edges, kept in a file beside the catalog. No query reads them unless asked.
    Overlay {
        #[command(subcommand)]
        action: OverlayAction,
    },
}

/// What `rllvm-query overlay` does.
#[derive(clap::Subcommand, Debug)]
pub enum OverlayAction {
    /// Read records from stdin, one JSON object per line, and append them all or none
    Record,
    /// List the current edges and the unresolved sites they cover
    List,
    /// Rewrite the file as the current edges, dropping their history
    Compact,
}

/// What `rllvm-query cache` does besides reporting.
#[derive(clap::Subcommand, Debug)]
pub enum CacheAction {
    /// Delete cached facts: every generation, or only stale ones
    Clear {
        /// Delete only generations this rllvm-query no longer reads
        #[arg(long)]
        stale: bool,
    },
}
