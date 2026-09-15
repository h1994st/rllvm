use std::{collections::HashMap, path::Path, process::ExitCode};

use clap::Parser;
use rllvm::{
    cli::{ClosureDirection, QueryArgs, QueryCommand},
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

/// Converts a parsed subcommand into the `query::Query` it names. Kept out of
/// `cli.rs` because `Query` does not exist without the `query` feature.
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
