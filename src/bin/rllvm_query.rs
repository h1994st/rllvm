use std::{collections::HashMap, path::Path, process::ExitCode};

use clap::Parser;
use rllvm::{
    cli::{ClosureDirection, QueryArgs, QueryCommand},
    config::try_rllvm_config,
    error::Error,
    query::{
        self, Query,
        bind::bind,
        extract::extract,
        facts::{CallSiteFact, FunctionFact, ModuleAnalysis, ProgramFacts, UseFact},
        index::{Direction, Session},
        load::{for_each_module, load_catalog},
        run,
    },
};
use tracing_subscriber::FmtSubscriber;

/// Converts a parsed subcommand into the `query::Query` it names. Kept out of
/// `cli.rs` because `Query` does not exist without the `query` feature.
///
/// Exhaustive over `QueryCommand`, so a new `QueryCommand` variant with no
/// arm here fails to compile. That alone does not catch the opposite drift --
/// a new `query::Query` variant added without a matching `QueryCommand` --
/// which compiles cleanly on its own. `cli_command_for` in this file's tests
/// closes that gap: it is exhaustive over `Query`, so a new `Query` variant
/// fails to compile there instead, until this file is updated to drive it
/// from the command line.
fn to_query(command: QueryCommand, heuristics: bool) -> Query {
    match command {
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
    }
}

/// Loads the catalog, extracts every module it names, binds cross-module
/// symbols, and returns a session ready to answer queries.
///
/// Assembly order matters, and enforces two spec rules. First, only
/// extraction may promote a module's report from `Verified` (set by
/// `load_catalog`) to `Analyzed`; a module that fails to extract is marked
/// `Failed` with the diagnostic instead, and the run continues -- the other
/// modules still answer. Second, `for_each_module` hands over one module's
/// bytes at a time by design, and the `Loaded` value (with the archive cache
/// `for_each_module` uses internally, already gone once it returns) is
/// dropped below before the session is built, so no bitcode buffer stays
/// resident once queries start answering.
fn build_session(catalog: &Path) -> Result<Session, Error> {
    let loaded = load_catalog(catalog)?;

    // Every module the loader intends to read, regardless of whether
    // extraction later succeeds: `bind` only consults a module's
    // configuration when it also sees a `FunctionFact` from that module, so
    // an entry for a module that fails extraction is simply unused.
    let configurations: HashMap<String, Option<String>> = loaded
        .pending
        .iter()
        .map(|module| (module.id.clone(), module.record.configuration_id.clone()))
        .collect();

    let mut functions: Vec<FunctionFact> = Vec::new();
    let mut call_sites: Vec<CallSiteFact> = Vec::new();
    let mut uses: Vec<UseFact> = Vec::new();
    let mut reports = loaded.reports.clone();

    for_each_module(&loaded, |module| {
        match extract(&module, &loaded.source_status) {
            Ok(facts) => {
                if let Some(report) = reports.iter_mut().find(|report| report.id == module.id) {
                    report.status = ModuleAnalysis::Analyzed;
                    if !facts.diagnostics.is_empty() {
                        let joined = facts.diagnostics.join("; ");
                        report.diagnostic = Some(match report.diagnostic.take() {
                            Some(existing) => format!("{existing}; {joined}"),
                            None => joined,
                        });
                    }
                }
                functions.extend(facts.functions);
                call_sites.extend(facts.call_sites);
                uses.extend(facts.uses);
            }
            Err(error) => {
                tracing::warn!(module = %module.id, %error, "module failed to extract");
                if let Some(report) = reports.iter_mut().find(|report| report.id == module.id) {
                    report.status = ModuleAnalysis::Failed;
                    report.diagnostic = Some(error.to_string());
                }
            }
        }
        // A module that fails to extract must not abort the run.
        Ok(())
    })?;

    let bindings = bind(&functions, &configurations);
    let facts = ProgramFacts {
        functions,
        call_sites,
        uses,
        scope: loaded.scope.clone(),
        origin: loaded.origin.clone(),
        modules: reports,
    };
    // The loop above already dropped its own archive cache on return; this
    // drops `Loaded` itself before the session below starts serving.
    drop(loaded);

    Ok(Session::new(facts, bindings))
}

fn run_query(args: QueryArgs) -> Result<(), Error> {
    let Some(command) = args.command else {
        return Ok(());
    };
    let catalog = args.catalog.ok_or_else(|| {
        Error::InvalidArguments("--catalog is required to run a query".to_string())
    })?;

    // Matches the wrapper binaries' own convention (`rllvm_cc.rs`,
    // `rllvm_get_bc.rs`, `rllvm_rustc.rs`): the configured log level, on
    // stderr, so `tracing::warn!` above (a module that failed to extract)
    // actually reaches a reader instead of being silently dropped by the
    // default no-op subscriber. Deferred until here, after both early
    // returns above, so `--llvm-version` alone never touches the
    // configuration file.
    FmtSubscriber::builder()
        .with_max_level(try_rllvm_config()?.log_level())
        .with_writer(std::io::stderr)
        .init();

    let session = build_session(&catalog)?;
    let query = to_query(command, args.heuristics);
    let result = run(&session, &query);

    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| Error::InvalidArguments(error.to_string()))?;
    println!("{json}");
    Ok(())
}

fn main() -> ExitCode {
    let args = QueryArgs::parse();
    if args.llvm_version {
        println!("{}", query::llvm_version());
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

    /// The reverse of `to_query`'s exhaustiveness: every `query::Query`
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
            let round_tripped = to_query(command, heuristics);
            assert_eq!(
                format!("{round_tripped:?}"),
                format!("{query:?}"),
                "CLI round-trip must preserve every field"
            );
        }
    }
}
