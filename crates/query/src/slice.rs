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
    Query, QueryResult, QueryResults,
    extract::extract_neutral,
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
/// A slice member that is only a declaration in its module contributes
/// nothing: the definition it binds to is a member of its own. Module bytes
/// are read and hash-checked as a query reads them, archive members
/// included, and written to a temporary directory under their position in
/// the catalog, never under a module id, which is free text.
pub fn emit_module(
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

    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for id in functions {
        wanted
            .entry(id.module_id.as_str())
            .or_default()
            .insert(id.symbol.as_str());
    }

    let loaded = load_catalog(catalog)?;
    let scratch = tempfile::TempDir::new()?;
    let mut archives = ArchiveCache::default();
    let mut pieces: Vec<Piece> = Vec::new();
    for (index, pending) in loaded.pending.iter().enumerate() {
        let Some(symbols) = wanted.remove(pending.id.as_str()) else {
            continue;
        };
        let module = read_module(pending, &mut archives)?;
        // Which members this module defines, and which of those are
        // aliases: `llvm-extract` names the two kinds with different flags,
        // and a declaration is not cut out at all.
        let facts = extract_neutral(&module)?;
        let mut piece = Piece {
            module: pending.id.clone(),
            input: scratch.path().join(format!("{index}.bc")),
            output: scratch.path().join(format!("{index}.slice.bc")),
            definitions: Vec::new(),
            statics: Vec::new(),
            named: facts
                .functions
                .iter()
                .map(|function| function.id.symbol.clone())
                .collect(),
        };
        for function in facts
            .functions
            .iter()
            .filter(|function| function.is_definition)
            .filter(|function| symbols.contains(function.id.symbol.as_str()))
        {
            let flag = if function.alias_of.is_some() {
                "--alias"
            } else {
                "--func"
            };
            piece.definitions.push((flag, function.id.symbol.clone()));
            if function.linkage == Linkage::Internal {
                piece.statics.push(function.id.symbol.clone());
            }
        }
        if !piece.definitions.is_empty() {
            std::fs::write(&piece.input, &module.bytes)?;
            pieces.push(piece);
        }
    }
    drop(archives);

    if let Some((module, _)) = wanted.into_iter().next() {
        return Err(Error::InvalidArguments(format!(
            "module {module} holds slice functions but was not read from {}",
            catalog.display()
        )));
    }
    if pieces.is_empty() {
        return Err(Error::InvalidArguments(
            "the slice holds no definition; nothing to emit".to_string(),
        ));
    }
    refuse_colliding_statics(&pieces)?;

    let environment = inherited_environment();
    for piece in &pieces {
        let mut arguments: Vec<OsString> = Vec::new();
        for (flag, symbol) in &piece.definitions {
            arguments.extend([OsString::from(flag), OsString::from(symbol)]);
        }
        arguments.extend([
            "-o".into(),
            piece.output.clone().into(),
            piece.input.clone().into(),
        ]);
        run_tool(&llvm_extract, &arguments, scratch.path(), &environment)?;
    }

    // The tools run in the scratch directory, so a relative `out` would
    // land there and vanish with it.
    let mut arguments: Vec<OsString> = vec!["-o".into(), std::path::absolute(out)?.into()];
    arguments.extend(
        pieces
            .iter()
            .map(|piece| piece.output.clone().into_os_string()),
    );
    run_tool(llvm_link, &arguments, scratch.path(), &environment)?;
    Ok(EmittedModule {
        path: out.to_path_buf(),
        modules: pieces.len(),
        functions: pieces.iter().map(|piece| piece.definitions.len()).sum(),
    })
}

/// [`emit_module`] for a `slice` answer. A slice that found no path is an
/// error, never an empty module.
pub fn emit_slice(
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
            emit_module(catalog, &slice.functions, llvm_link, out)
        }
        _ => Err(Error::InvalidArguments(
            "only a `slice` answer emits a module".to_string(),
        )),
    }
}

/// One source module's share of the slice, before `llvm-extract` runs.
struct Piece {
    module: String,
    /// The module's bytes, named after its position in the catalog.
    input: PathBuf,
    output: PathBuf,
    /// Each definition cut out, with the `llvm-extract` flag that names it:
    /// `--func`, or `--alias` for an alias.
    definitions: Vec<(&'static str, String)>,
    /// The definitions cut out that were `static`.
    statics: Vec<String>,
    /// Every function the module defines or declares.
    named: BTreeSet<String>,
}

/// `llvm-extract` makes every local function it keeps external, so a
/// `static` cut out of one module would be linked as the definition of any
/// same-named function another piece defines or calls: a module that binds
/// calls the program never makes. Refused, naming both modules.
fn refuse_colliding_statics(pieces: &[Piece]) -> Result<(), Error> {
    for piece in pieces {
        for symbol in &piece.statics {
            if let Some(other) = pieces
                .iter()
                .find(|other| other.module != piece.module && other.named.contains(symbol))
            {
                return Err(Error::InvalidArguments(format!(
                    "cannot emit the slice as one module: `{symbol}` is static in module {} \
                     and also named in module {}, and llvm-extract makes a static it keeps \
                     external",
                    piece.module, other.module
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
