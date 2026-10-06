//! Cutting a slice's definitions out of the catalog's modules into one
//! module, for scoped verification or a closer read.
//!
//! The work is done by `llvm-extract` and `llvm-link`, run as commands: the
//! LLVM handles that would do it in process belong to `extract.rs` alone.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::Serialize;

use rllvm_core::{catalog::ArchiveCache, error::Error, utils::execute_llvm_tool_in_for_output};

use crate::{
    Query, QueryResult, QueryResults, Session,
    facts::{FunctionId, Linkage},
    load::{load_catalog, read_module},
};

/// `llvm-extract`'s file name, looked for beside the configured `llvm-link`.
const LLVM_EXTRACT: &str = "llvm-extract";

/// What [`emit_module`] wrote.
#[derive(Debug, Serialize)]
pub struct EmittedModule {
    pub path: PathBuf,
    /// Source modules that contributed a definition.
    pub modules: usize,
    /// Definitions extracted, aliases included.
    pub functions: usize,
}

/// Writes one module holding the slice's definitions: each source module's
/// slice functions are cut out with `llvm-extract --func` (`--alias` for an
/// alias), then the pieces are joined with `llvm-link`. Other functions
/// stay as declarations. `llvm-extract` is the sibling of the configured
/// `llvm-link`.
///
/// `session` is the one `functions` came from, loaded from `catalog`: it says
/// which members are definitions, which are aliases, and which functions
/// each module names, so no module is parsed again here. A slice member that
/// is only a declaration contributes nothing: the definition it binds to is
/// a member of its own. Module bytes are read and hash-checked
/// as a query reads them, archive members included, and written to a
/// temporary directory under their position in the catalog, never under a
/// module id, which is free text.
///
/// `llvm-extract` makes every `static` it keeps external, so a `static` cut
/// out that shares a name with a function another contributing module names
/// is refused rather than linked.
pub fn emit_module(
    session: &Session,
    catalog: &Path,
    functions: &[FunctionId],
    llvm_link: &Path,
    out: &Path,
) -> Result<EmittedModule, Error> {
    let llvm_extract = llvm_link.with_file_name(LLVM_EXTRACT);
    if !llvm_extract.is_file() {
        return Err(Error::MissingFile(format!(
            "`{LLVM_EXTRACT}` is needed beside the configured llvm-link, at {}",
            llvm_extract.display()
        )));
    }

    let mut plans = plan_pieces(session, functions);
    if plans.is_empty() {
        return Err(Error::InvalidArguments(
            "the slice holds no definition; nothing to emit".to_string(),
        ));
    }
    refuse_colliding_statics(&plans)?;

    let loaded = load_catalog(catalog)?;
    let scratch = tempfile::TempDir::new()?;
    let mut archives = ArchiveCache::default();
    let mut pieces: Vec<(PathBuf, PathBuf, Plan)> = Vec::new();
    for (index, pending) in loaded.pending.iter().enumerate() {
        let Some(plan) = plans.remove(pending.id.as_str()) else {
            continue;
        };
        // The facts above describe the bytes the session read; a catalog
        // rewritten since names other bytes, which they do not describe.
        let analyzed = session
            .modules()
            .iter()
            .find(|report| report.id == pending.id)
            .and_then(|report| report.content_sha256.as_ref());
        if analyzed.is_some() && analyzed != pending.record.content_sha256.as_ref() {
            return Err(Error::InvalidArguments(format!(
                "module {} changed in {} since the query read it",
                pending.id,
                catalog.display()
            )));
        }
        let module = read_module(pending, &mut archives)?;
        let input = scratch.path().join(format!("{index}.bc"));
        let output = scratch.path().join(format!("{index}.slice.bc"));
        std::fs::write(&input, &module.bytes)?;
        pieces.push((input, output, plan));
    }
    drop(archives);
    if let Some(module) = plans.into_keys().next() {
        return Err(Error::InvalidArguments(format!(
            "module {module} holds slice definitions but was not read from {}",
            catalog.display()
        )));
    }

    let environment = inherited_environment();
    for (input, output, plan) in &pieces {
        let mut arguments: Vec<OsString> = Vec::new();
        for (flag, symbol) in &plan.definitions {
            arguments.extend([OsString::from(flag), OsString::from(symbol)]);
        }
        arguments.extend(["-o".into(), output.clone().into(), input.clone().into()]);
        run_tool(&llvm_extract, &arguments, scratch.path(), &environment)?;
    }

    // The tools run in the scratch directory, so a relative `out` would
    // land there and vanish with it.
    let mut arguments: Vec<OsString> = vec!["-o".into(), std::path::absolute(out)?.into()];
    arguments.extend(
        pieces
            .iter()
            .map(|(_, output, _)| output.clone().into_os_string()),
    );
    run_tool(llvm_link, &arguments, scratch.path(), &environment)?;
    Ok(EmittedModule {
        path: out.to_path_buf(),
        modules: pieces.len(),
        functions: pieces
            .iter()
            .map(|(_, _, plan)| plan.definitions.len())
            .sum(),
    })
}

/// [`emit_module`] for a `slice` answer `session` gave. A slice that found
/// no path is an error, never an empty module.
pub fn emit_slice(
    session: &Session,
    catalog: &Path,
    answer: &QueryResult,
    llvm_link: &Path,
    out: &Path,
) -> Result<EmittedModule, Error> {
    match (&answer.query, &answer.results) {
        (Query::Slice { from, to, .. }, QueryResults::Slice(slice)) => {
            if slice.functions.is_empty() {
                return Err(Error::InvalidArguments(format!(
                    "no path from {from} to {to}; nothing to emit"
                )));
            }
            emit_module(session, catalog, &slice.functions, llvm_link, out)
        }
        _ => Err(Error::InvalidArguments(
            "only a `slice` answer emits a module".to_string(),
        )),
    }
}

/// What one source module contributes, decided from the session's facts
/// before any bytes are read. Keyed by module id; a module whose members are
/// all declarations has no entry.
fn plan_pieces(session: &Session, functions: &[FunctionId]) -> BTreeMap<String, Plan> {
    let mut plans: BTreeMap<String, Plan> = BTreeMap::new();
    for id in functions {
        let Some(function) = session.function(id).filter(|f| f.is_definition) else {
            continue;
        };
        let plan = plans.entry(id.module_id.clone()).or_default();
        let flag = if function.alias_of.is_some() {
            "--alias"
        } else {
            "--func"
        };
        if plan.cut.insert(function.id.symbol.clone()) {
            plan.definitions.push((flag, function.id.symbol.clone()));
        }
    }
    for function in session.functions() {
        if let Some(plan) = plans.get_mut(&function.id.module_id) {
            plan.named.insert(function.id.symbol.clone());
            if function.linkage == Linkage::Internal && plan.cut.contains(&function.id.symbol) {
                plan.statics.insert(function.id.symbol.clone());
            }
        }
    }
    plans
}

/// One source module's share of the slice.
#[derive(Default)]
struct Plan {
    /// Each definition cut out, with the `llvm-extract` flag that names it:
    /// `--func`, or `--alias` for an alias.
    definitions: Vec<(&'static str, String)>,
    /// The symbols in `definitions`.
    cut: BTreeSet<String>,
    /// The definitions cut out that were `static`.
    statics: BTreeSet<String>,
    /// Every function the module defines or declares.
    named: BTreeSet<String>,
}

/// `llvm-extract` makes every local function it keeps external, so a
/// `static` cut out of one module would be linked as the definition of any
/// same-named function another piece defines or calls: a module that binds
/// calls the program never makes. Refused, naming both modules.
///
/// Conservative: it compares every function each module names, not only what
/// survives in its piece.
fn refuse_colliding_statics(plans: &BTreeMap<String, Plan>) -> Result<(), Error> {
    for (module, plan) in plans {
        for symbol in &plan.statics {
            if let Some((other, _)) = plans
                .iter()
                .find(|(other, other_plan)| *other != module && other_plan.named.contains(symbol))
            {
                return Err(Error::InvalidArguments(format!(
                    "cannot emit the slice as one module: `{symbol}` is static in module \
                     {module} and also named in module {other}, and llvm-extract makes a \
                     static external"
                )));
            }
        }
    }
    Ok(())
}

/// Runs one LLVM tool through the shared transport, which moves a long
/// argument list into a response file, and fails with its stderr.
fn run_tool(
    tool: &Path,
    arguments: &[OsString],
    directory: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<(), Error> {
    let output = execute_llvm_tool_in_for_output(tool, arguments, directory, environment)?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::ExecutionFailure(format!(
        "{} failed ({}): {}",
        tool.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// This process's environment, for the transport helper, which replaces a
/// child's environment rather than adding to it. A variable that is not
/// UTF-8 cannot be carried and is left out.
fn inherited_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}
