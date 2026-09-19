use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use rllvm_core::{config::try_rllvm_config, error::Error};
use rllvm_query::{
    Query,
    cli::{ClosureDirection, QueryArgs, QueryCommand},
    index::Direction,
    llvm_version, mcp, open, run,
};
use tracing_subscriber::FmtSubscriber;

/// Converts a parsed subcommand into the [`Query`] it names, or `None` for
/// the two variants that name a mode rather than one of the nine queries:
/// `Mcp` (serve over MCP stdio) and `Completions` (print a completion
/// script), both of which this binary has already handled by the time a
/// query would run.
///
/// Exhaustive over `QueryCommand`, so a new `QueryCommand` variant with no
/// arm here fails to compile. That alone does not catch the opposite drift --
/// a new [`Query`] variant added without a matching `QueryCommand` --
/// which compiles cleanly on its own. `cli_command_for` in this file's tests
/// closes that gap: it is exhaustive over `Query`, so a new `Query` variant
/// fails to compile there instead, until this file is updated to drive it
/// from the command line.
fn to_query(command: QueryCommand, heuristics: bool) -> Option<Query> {
    Some(match command {
        QueryCommand::Defs { name } => Query::Defs { name },
        QueryCommand::At { file, line } => Query::At { file, line },
        QueryCommand::Callers { name } => Query::Callers { name },
        QueryCommand::Callees { name } => Query::Callees { name },
        QueryCommand::Uses { name } => Query::Uses { name },
        QueryCommand::Reach { from, to } => Query::Reach { from, to },
        QueryCommand::Closure { name, direction } => Query::Closure {
            name,
            direction: match direction {
                ClosureDirection::In => Direction::In,
                ClosureDirection::Out => Direction::Out,
            },
        },
        QueryCommand::Externals => Query::Externals,
        QueryCommand::IndirectTargets { at } => Query::IndirectTargets { at, heuristics },
        QueryCommand::Mcp => return None,
        // `main` answers this one before `run_query` is ever called; the arm
        // is here so adding a mode variant cannot compile without a decision.
        QueryCommand::Completions { .. } => return None,
    })
}

fn run_query(args: QueryArgs) -> Result<(), Error> {
    let Some(command) = args.command else {
        return Ok(());
    };

    // Matches the wrapper binaries' own convention (`rllvm_cc.rs`,
    // `rllvm_get_bc.rs`, `rllvm_rustc.rs`): the configured log level, on
    // stderr, so `tracing::warn!` above (a module that failed to extract)
    // actually reaches a reader instead of being silently dropped by the
    // default no-op subscriber. Deferred until here, after the early return
    // above, so `--llvm-version` alone never touches the configuration file.
    FmtSubscriber::builder()
        .with_max_level(try_rllvm_config()?.log_level())
        .with_writer(std::io::stderr)
        .init();

    // The dispatch, named rather than inferred from `to_query`'s `None`.
    // Two commands answer `None` now -- `Mcp` and `Completions` -- so `None`
    // no longer identifies which mode was asked for, and reading it as "serve
    // MCP" would make the next mode-shaped variant silently do that: the one
    // outcome nobody would think to test for. Exhaustive and wildcard-free,
    // so such a variant has to fail to compile here too, not only in
    // `to_query`, where its `None` arm would otherwise be the whole story.
    match &command {
        QueryCommand::Mcp => return serve_mcp(args.catalog.as_deref()),
        // `main` answers this one before `run_query` is reached, so arriving
        // here means that early return was dropped --
        // `completions_name_the_query_binary` fails when it is, because
        // nothing reaches stdout.
        QueryCommand::Completions { .. } => return Ok(()),
        QueryCommand::Defs { .. }
        | QueryCommand::At { .. }
        | QueryCommand::Callers { .. }
        | QueryCommand::Callees { .. }
        | QueryCommand::Uses { .. }
        | QueryCommand::Reach { .. }
        | QueryCommand::Closure { .. }
        | QueryCommand::Externals
        | QueryCommand::IndirectTargets { .. } => {}
    }

    // Unreachable through the match above, which returns for every command
    // `to_query` answers `None` for. An error rather than a panic: a binary
    // that mis-dispatches should say so, not abort.
    let Some(query) = to_query(command, args.heuristics) else {
        return Err(Error::InvalidArguments(
            "this command names a mode, not a query".to_string(),
        ));
    };

    // Before `open`, so a mistyped location costs a diagnostic rather than a
    // full read and extraction of the catalog.
    query.validate()?;
    let catalog = args.catalog.ok_or_else(|| {
        Error::InvalidArguments("--catalog is required to run a query".to_string())
    })?;
    let result = run(&open(&catalog)?, &query)?;

    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| Error::InvalidArguments(error.to_string()))?;
    println!("{json}");
    Ok(())
}

/// Serves MCP over stdio. `--catalog` is optional here and only preloads:
/// the point of the server is that a client chooses what to analyse, through
/// `load_catalog` and `inventory`, and keeps several catalogs loaded at once.
/// A preload failure is still fatal -- a client that asked for a catalog on
/// the command line should hear that it could not be read, not discover it
/// one query later.
fn serve_mcp(catalog: Option<&std::path::Path>) -> Result<(), Error> {
    let mut registry = mcp::Registry::new();
    if let Some(catalog) = catalog {
        registry.load(catalog)?;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    mcp::serve(&mut registry, stdin.lock(), stdout.lock())
}

fn main() -> ExitCode {
    let args = QueryArgs::parse();
    if args.llvm_version {
        // Documented as "print ... and exit": must return here rather than
        // falling into `run_query`, or `--llvm-version --catalog c mcp`
        // would print a bare version line onto stdout ahead of the
        // JSON-RPC frames, corrupting the protocol stream.
        println!("{}", llvm_version());
        return ExitCode::SUCCESS;
    }
    // Also before any configuration is read: generating a completion script
    // is a property of the CLI definition alone, and must not fail on a
    // machine that has no usable LLVM configuration yet.
    if let Some(QueryCommand::Completions { shell }) = args.command {
        let mut command = QueryArgs::command();
        let name = command.get_name().to_string();
        clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    match run_query(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reverse of `to_query`'s exhaustiveness: every [`Query`]
    /// variant must appear here, with no wildcard arm. A `Query` variant
    /// added without a matching arm fails to compile, so a new query cannot
    /// ship without a `QueryCommand` (and a `to_query` arm) to drive it from
    /// the command line -- closing the drift gap `to_query` alone leaves
    /// open, since it is only ever exhaustive over `QueryCommand`.
    fn cli_command_for(query: &Query) -> QueryCommand {
        match query {
            Query::Defs { name } => QueryCommand::Defs { name: name.clone() },
            Query::At { file, line } => QueryCommand::At {
                file: file.clone(),
                line: *line,
            },
            Query::Callers { name } => QueryCommand::Callers { name: name.clone() },
            Query::Callees { name } => QueryCommand::Callees { name: name.clone() },
            Query::Uses { name } => QueryCommand::Uses { name: name.clone() },
            Query::Reach { from, to } => QueryCommand::Reach {
                from: from.clone(),
                to: to.clone(),
            },
            Query::Closure { name, direction } => QueryCommand::Closure {
                name: name.clone(),
                direction: match direction {
                    Direction::In => ClosureDirection::In,
                    Direction::Out => ClosureDirection::Out,
                },
            },
            Query::Externals => QueryCommand::Externals,
            Query::IndirectTargets { at, .. } => QueryCommand::IndirectTargets { at: at.clone() },
        }
    }

    /// `to_query` maps `QueryCommand::Mcp` to `None`, since it selects a
    /// mode rather than naming a query.
    #[test]
    fn the_mcp_command_has_no_query() {
        assert!(to_query(QueryCommand::Mcp, false).is_none());
    }

    /// Same for `Completions`: the binary answers it before a query could
    /// run, so reaching `to_query` with it must not name one.
    #[test]
    fn the_completions_command_has_no_query() {
        let command = QueryCommand::Completions {
            shell: clap_complete::Shell::Bash,
        };
        assert!(to_query(command, false).is_none());
    }

    /// Not just a compile-time fence: proves `to_query` and `cli_command_for`
    /// actually agree on every field, for every variant, not merely that
    /// both happen to be exhaustive.
    #[test]
    fn every_query_variant_round_trips_through_the_cli_command_mapping() {
        let heuristics = true;
        let queries = [
            Query::Defs { name: "f".into() },
            Query::At {
                file: "t.c".into(),
                line: 1,
            },
            Query::Callers { name: "f".into() },
            Query::Callees { name: "f".into() },
            Query::Uses { name: "f".into() },
            Query::Reach {
                from: "a".into(),
                to: "b".into(),
            },
            Query::Closure {
                name: "f".into(),
                direction: Direction::In,
            },
            Query::Closure {
                name: "f".into(),
                direction: Direction::Out,
            },
            Query::Externals,
            Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics,
            },
        ];
        for query in queries {
            let command = cli_command_for(&query);
            let round_tripped =
                to_query(command, heuristics).expect("every QueryCommand but Mcp names a query");
            assert_eq!(
                format!("{round_tripped:?}"),
                format!("{query:?}"),
                "CLI round-trip must preserve every field"
            );
        }
    }
}
