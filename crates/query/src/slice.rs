//! Cutting a slice's definitions out of the catalog's modules into one
//! module, for scoped verification or a closer read.
//!
//! The work is done by `llvm-extract` and `llvm-link`, run as commands: the
//! LLVM handles that would do it in process belong to `extract.rs` alone.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use serde::Serialize;

use rllvm_core::{catalog::ArchiveCache, error::Error, utils::execute_llvm_tool_in_for_output};

use crate::{
    Query, QueryResult, QueryResults, Session,
    facts::FunctionId,
    load::{load_catalog, read_module},
};

/// `llvm-extract`'s file name, looked for beside the configured `llvm-link`.
const LLVM_EXTRACT: &str = "llvm-extract";

/// `llvm-nm`'s file name, looked for beside the configured `llvm-link`.
const LLVM_NM: &str = "llvm-nm";

/// The `llvm-nm` type letters of a symbol local to its module: a `static`
/// function, variable or constant. Lowercase `w`, `v` and `u` are not local.
const LOCAL_SYMBOL_TYPES: &[char] = &['t', 'd', 'b', 'r', 's'];

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
/// stay as declarations. `llvm-extract` and `llvm-nm` are the siblings of
/// the configured `llvm-link`.
///
/// `session` is the one `functions` came from, loaded from `catalog`: it says
/// which members are definitions and which are aliases, so no module is
/// parsed again here. A slice member that is only a declaration contributes
/// nothing: the definition it binds to is a member of its own. An alias has no body, so it comes with the function
/// it stands for, on the path or not. Module bytes are read and hash-checked
/// as a query reads them, archive members included, and written to a
/// temporary directory under their position in the catalog, never under a
/// module id, which is free text.
///
/// `llvm-extract` makes every `static` that stays in a piece external, as a
/// definition or as a declaration a kept function still uses, and
/// `llvm-link` then binds it by name. So every piece's symbols, functions
/// and data, defined and undefined, are listed with the `llvm-nm` beside
/// `llvm-link`, and a `static` of one module that stays in its piece under a
/// name another piece also holds is refused rather than linked. What remains
/// is compiler-private data such as string literals, which `llvm-nm` does
/// not list for its source module: it too becomes an external declaration,
/// and same-named ones from two pieces merge into one declaration.
pub fn emit_module(
    session: &Session,
    catalog: &Path,
    functions: &[FunctionId],
    llvm_link: &Path,
    out: &Path,
) -> Result<EmittedModule, Error> {
    let llvm_extract = sibling(llvm_link, LLVM_EXTRACT)?;
    let llvm_nm = sibling(llvm_link, LLVM_NM)?;

    let mut plans = plan_pieces(session, functions);
    if plans.is_empty() {
        return Err(Error::InvalidArguments(
            "the slice holds no definition; nothing to emit".to_string(),
        ));
    }

    let loaded = load_catalog(catalog)?;
    let scratch = tempfile::TempDir::new()?;
    let mut archives = ArchiveCache::default();
    let mut pieces: Vec<Piece> = Vec::new();
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
        let piece = Piece {
            module: pending.id.clone(),
            input: scratch.path().join(format!("{index}.bc")),
            output: scratch.path().join(format!("{index}.slice.bc")),
            plan,
            promoted: BTreeSet::new(),
            symbols: BTreeSet::new(),
        };
        std::fs::write(&piece.input, &module.bytes)?;
        pieces.push(piece);
    }
    drop(archives);
    if let Some(module) = plans.into_keys().next() {
        return Err(Error::InvalidArguments(format!(
            "module {module} holds slice definitions but was not read from {}",
            catalog.display()
        )));
    }

    let environment = inherited_environment();
    for piece in &mut pieces {
        let mut arguments: Vec<OsString> = Vec::new();
        for (flag, symbol) in &piece.plan.definitions {
            arguments.extend([OsString::from(flag), OsString::from(symbol)]);
        }
        arguments.extend([
            "-o".into(),
            piece.output.clone().into(),
            piece.input.clone().into(),
        ]);
        run_tool(&llvm_extract, &arguments, scratch.path(), &environment)?;

        let list = |file: &Path| {
            run_tool(&llvm_nm, &[file.as_os_str()], scratch.path(), &environment)
                .map(|stdout| symbol_table(&String::from_utf8_lossy(&stdout)))
        };
        let statics: BTreeSet<String> = list(&piece.input)?
            .into_iter()
            .filter(|(kind, _)| LOCAL_SYMBOL_TYPES.contains(kind))
            .map(|(_, name)| name)
            .collect();
        piece.symbols = list(&piece.output)?
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        piece.promoted = piece.symbols.intersection(&statics).cloned().collect();
    }
    refuse_colliding_statics(&pieces)?;

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
        functions: pieces
            .iter()
            .map(|piece| piece.plan.definitions.len())
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
        // An alias, then what it stands for, until a function with a body:
        // `llvm-extract` keeps an alias whose aliasee it cut away as a
        // declaration, which no linker accepts.
        let mut next = Some(function);
        while let Some(function) = next {
            let flag = if function.alias_of.is_some() {
                "--alias"
            } else {
                "--func"
            };
            if !plan.cut.insert(function.id.symbol.clone()) {
                break;
            }
            plan.definitions.push((flag, function.id.symbol.clone()));
            next = function
                .alias_of
                .as_ref()
                .and_then(|target| session.function(target));
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
}

/// One source module's share of the slice, as files.
struct Piece {
    module: String,
    /// The module's bytes, named after its position in the catalog.
    input: PathBuf,
    output: PathBuf,
    plan: Plan,
    /// Every symbol `llvm-nm` lists in `output`, as it spells them.
    symbols: BTreeSet<String>,
    /// The symbols in `symbols` that were `static` in `input`.
    promoted: BTreeSet<String>,
}

/// A `static` that `llvm-extract` made external would be linked to a
/// same-named function or variable in another piece: a module that binds
/// calls, or reads, the program never makes. Refused, naming both modules.
fn refuse_colliding_statics(pieces: &[Piece]) -> Result<(), Error> {
    for piece in pieces {
        for symbol in &piece.promoted {
            if let Some(other) = pieces
                .iter()
                .find(|other| other.module != piece.module && other.symbols.contains(symbol))
            {
                return Err(Error::InvalidArguments(format!(
                    "cannot emit the slice as one module: `{symbol}` is static in module {} \
                     and also named by module {}'s part, and llvm-extract makes a static it \
                     keeps external",
                    piece.module, other.module
                )));
            }
        }
    }
    Ok(())
}

/// `llvm-nm`'s plain listing as `(type letter, name)` pairs. A line is an
/// optional value, the letter and the name; anything else is skipped.
fn symbol_table(listing: &str) -> Vec<(char, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().rev();
            let name = fields.next()?;
            let mut letter = fields.next()?.chars();
            match (letter.next(), letter.next()) {
                (Some(kind), None) => Some((kind, name.to_string())),
                _ => None,
            }
        })
        .collect()
}

/// The tool named `name` in `llvm_link`'s directory.
fn sibling(llvm_link: &Path, name: &str) -> Result<PathBuf, Error> {
    let tool = llvm_link.with_file_name(name);
    if tool.is_file() {
        return Ok(tool);
    }
    Err(Error::MissingFile(format!(
        "`{name}` is needed beside the configured llvm-link, at {}",
        tool.display()
    )))
}

/// Runs one LLVM tool through the shared transport, which moves a long
/// argument list into a response file. Answers its stdout, or fails with its
/// stderr.
fn run_tool<S: AsRef<OsStr>>(
    tool: &Path,
    arguments: &[S],
    directory: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<Vec<u8>, Error> {
    let output = execute_llvm_tool_in_for_output(tool, arguments, directory, environment)?;
    if output.status.success() {
        return Ok(output.stdout);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_reads_as_type_letters_and_names() {
        let listing = "---------------- t _g\n                 U _leaf\n\
                       ---------------- d _counter\n0000000000000000 T main\n\nlib.a:\n";
        assert_eq!(
            symbol_table(listing),
            [
                ('t', "_g".to_string()),
                ('U', "_leaf".to_string()),
                ('d', "_counter".to_string()),
                ('T', "main".to_string()),
            ]
        );
    }
}
