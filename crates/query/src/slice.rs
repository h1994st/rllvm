//! Cutting a slice's definitions out of the catalog's modules into one
//! module, for scoped verification or a closer read.
//!
//! The work is done by `llvm-extract` and `llvm-link`, run as commands: the
//! LLVM handles that would do it in process belong to `extract.rs` alone.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Output,
};

use serde::Serialize;

use rllvm_core::{
    catalog::ArchiveCache,
    config::LlvmBindir,
    diagnostics::{llvm_major_version, tool_llvm_major_version},
    error::Error,
    utils::execute_llvm_tool_in_for_output,
};

use crate::{
    Query, QueryResult, QueryResults, Session,
    extract::llvm_version,
    facts::{FunctionId, Linkage},
    load::{PendingModule, load_catalog, read_module},
};

/// `llvm-extract`'s file name, looked for in the LLVM bindir.
const LLVM_EXTRACT: &str = "llvm-extract";

/// `opt`'s file name, looked for in the LLVM bindir.
const OPT: &str = "opt";

/// What [`promote`] runs `opt` with. Clang marks every `-O0` function
/// `optnone`, which `mem2reg` skips, so the attribute is removed first.
const PROMOTE_ARGUMENTS: &[&str] = &[
    "-force-remove-attribute=optnone",
    "-passes=forceattrs,mem2reg",
];

/// What names the LLVM release in a producer string: clang's. Other
/// producers, rustc's `rustc version 1.x`, number their own releases.
const CLANG_PRODUCER: &str = "clang version";

/// `llvm-nm`'s file name, looked for in the LLVM bindir.
const LLVM_NM: &str = "llvm-nm";

/// The `llvm-nm` type letters of a symbol local to its module: a `static`
/// function, variable or constant. Lowercase `w`, `v` and `u` are not local.
const LOCAL_SYMBOL_TYPES: &[char] = &['t', 'd', 'b', 'r', 's'];

/// What a target may put before every IR name in its symbol table: nothing
/// on ELF, `_` on Mach-O.
const GLOBAL_PREFIXES: &[&str] = &["", "_"];

/// Marks an IR name the target must spell exactly as written, unprefixed:
/// an Objective-C method's, or one given with `asm`.
const VERBATIM_NAME: char = '\u{1}';

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
/// stay as declarations. `llvm-extract` and `llvm-nm` come from
/// `llvm_bindir`, which need not hold `llvm-link`.
///
/// `session` is the one `functions` came from, loaded from `catalog`: it says
/// which members are definitions and which are aliases, so no module is
/// parsed again here. A slice member that is only a declaration contributes
/// nothing: the definition it binds to is a member of its own. An alias has
/// no body, so it comes with the function it stands for, on the path or not.
/// Module bytes are read and hash-checked as a query reads them, archive
/// members included, and written to a temporary directory under their
/// position in the catalog, never under a module id, which is free text.
///
/// `llvm-extract` makes every `static` that stays in a piece external, as a
/// definition or as a declaration a kept function still uses, and
/// `llvm-link` then binds it by name. So every piece's symbols, functions
/// and data, defined and undefined, are listed with `llvm_bindir`'s
/// `llvm-nm`, and a `static` of one module that stays in its piece under a
/// name another piece also holds is refused rather than linked. A `static`
/// is what the source module's listing calls local, or a function the facts
/// give internal or private linkage: `llvm-nm` omits private symbols, so the
/// facts name those, spelled with the prefix the piece's own listing shows.
/// What remains is compiler-private data such as string literals, in neither
/// list: it too becomes an external declaration, and same-named ones from two
/// pieces merge into one declaration.
pub fn emit_module(
    session: &Session,
    catalog: &Path,
    functions: &[FunctionId],
    llvm_link: &Path,
    llvm_bindir: &LlvmBindir,
    out: &Path,
) -> Result<EmittedModule, Error> {
    let llvm_extract = llvm_bindir.tool(LLVM_EXTRACT)?;
    let llvm_nm = llvm_bindir.tool(LLVM_NM)?;

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
        refuse_changed_module(session, pending, catalog)?;
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
        let mut statics: BTreeSet<String> = list(&piece.input)?
            .into_iter()
            .filter(|(kind, _)| LOCAL_SYMBOL_TYPES.contains(kind))
            .map(|(_, name)| name)
            .collect();
        piece.symbols = list(&piece.output)?
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        // A private function is internal in the facts but absent from the
        // listing, so the facts name it too, spelled as the target does.
        let prefix = global_prefix(&piece.plan.cut, &piece.symbols);
        for symbol in &piece.plan.internal {
            statics.extend(spellings(symbol, prefix));
        }
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

/// Refuses a module whose bytes changed in the catalog since `session` read
/// them: the facts describe those bytes, not the ones now on disk.
fn refuse_changed_module(
    session: &Session,
    pending: &PendingModule,
    catalog: &Path,
) -> Result<(), Error> {
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
    Ok(())
}

/// The LLVM major `module`'s producers name, the highest if several do.
/// `None` when none names one: rustc's, or Apple clang's.
fn producer_major(producers: &[String]) -> Option<u32> {
    producers
        .iter()
        .filter(|producer| producer.contains(CLANG_PRODUCER))
        .filter_map(|producer| llvm_major_version(producer))
        .max()
}

/// The major of the LLVM linked into this process.
fn linked_major() -> Option<u32> {
    llvm_version().split('.').next()?.parse().ok()
}

/// `opt`'s LLVM major, or why it cannot be established.
fn opt_major(opt: &Path) -> Result<u32, Error> {
    tool_llvm_major_version(opt).ok_or_else(|| {
        Error::InvalidArguments(format!(
            "cannot establish the LLVM version of {}; `{OPT} --version` did not report one",
            opt.display()
        ))
    })
}

/// Checks that `opt` can round-trip `module`: its LLVM major must reach the
/// producer's (or it cannot read the bitcode) without passing the LLVM
/// linked into this process (or that cannot read what `opt` writes). A
/// producer that names no LLVM release, such as rustc, is left to `opt`'s own
/// reader: [`promote_anchor_module`] names `opt`'s version if that fails.
pub fn check_opt_llvm(
    llvm_bindir: &LlvmBindir,
    session: &Session,
    module: &str,
) -> Result<(), Error> {
    let opt = llvm_bindir.tool(OPT)?;
    let opt_major = opt_major(&opt)?;
    if let Some(linked) = linked_major()
        && opt_major > linked
    {
        return Err(Error::InvalidArguments(format!(
            "{} is LLVM {opt_major}, newer than the LLVM {linked} rllvm-query is linked with, \
             which could not read what it writes; point `llvm_bindir` at LLVM {linked} or older",
            opt.display()
        )));
    }
    let producers = session
        .modules()
        .iter()
        .find(|report| report.id == module)
        .map(|report| report.producers.as_slice())
        .unwrap_or_default();
    if let Some(producer) = producer_major(producers)
        && opt_major < producer
    {
        return Err(Error::InvalidArguments(format!(
            "{} is LLVM {opt_major}, older than the LLVM {producer} that produced module \
             {module}, so it cannot read it; point `llvm_bindir` at LLVM {producer} or newer",
            opt.display()
        )));
    }
    Ok(())
}

/// A module [`promote`] wrote, and what `opt` reported while it did.
#[derive(Clone, Debug)]
pub struct PromotedModule {
    pub bytes: Vec<u8>,
    /// Each line `opt` wrote to stderr: a warning such as debug info it
    /// stripped on reading, which changes what the module still says.
    pub diagnostics: Vec<String>,
}

/// Runs `mem2reg` over `module_bytes` with `llvm_bindir`'s `opt`, in
/// `scratch`, and answers the promoted bitcode. Stack slots become SSA
/// values, so a guard on a `-O0` local is visible to a walk of the IR.
pub fn promote(
    module_bytes: &[u8],
    llvm_bindir: &LlvmBindir,
    scratch: &Path,
) -> Result<PromotedModule, Error> {
    let opt = llvm_bindir.tool(OPT)?;
    let input = scratch.join("promote.in.bc");
    let output = scratch.join("promote.out.bc");
    std::fs::write(&input, module_bytes)?;
    let mut arguments: Vec<OsString> = PROMOTE_ARGUMENTS.iter().map(OsString::from).collect();
    arguments.extend([
        input.into_os_string(),
        "-o".into(),
        output.clone().into_os_string(),
    ]);
    let stderr = run_tool_output(&opt, &arguments, scratch, &inherited_environment())?.stderr;
    Ok(PromotedModule {
        bytes: std::fs::read(output)?,
        diagnostics: String::from_utf8_lossy(&stderr)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

/// The module that defines `anchor`, `mem2reg`-promoted. Only
/// that one module is read, never linked with another. The catalog and the
/// LLVM bindir are the ones `session` was given; a session without either
/// cannot promote, and says which is missing.
pub fn promote_anchor_module(
    session: &Session,
    anchor: &FunctionId,
) -> Result<PromotedModule, Error> {
    let catalog = session.catalog_path().ok_or_else(|| {
        Error::InvalidArguments(
            "this session was not opened from a catalog file, so there are no module bytes to \
             promote"
                .to_string(),
        )
    })?;
    let llvm_bindir = match session.llvm_bindir() {
        Some(Ok(llvm_bindir)) => llvm_bindir,
        Some(Err(message)) => return Err(Error::InvalidArguments(message.clone())),
        None => {
            return Err(Error::InvalidArguments(
                "no LLVM bindir was configured for this session, and `opt` is needed to promote \
                 the module"
                    .to_string(),
            ));
        }
    };
    let module = &anchor.module_id;
    check_opt_llvm(llvm_bindir, session, module)?;

    let loaded = load_catalog(catalog)?;
    let pending = loaded
        .pending
        .iter()
        .find(|pending| pending.id == *module)
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "module {module} is not among the readable modules of {}",
                catalog.display()
            ))
        })?;
    refuse_changed_module(session, pending, catalog)?;
    let bytes = read_module(pending, &mut ArchiveCache::default())?.bytes;

    let scratch = tempfile::TempDir::new()?;
    promote(&bytes, llvm_bindir, scratch.path()).map_err(|error| {
        let version = llvm_bindir
            .tool(OPT)
            .ok()
            .and_then(|opt| tool_llvm_major_version(&opt))
            .map_or_else(|| "of unknown version".to_string(), |v| format!("LLVM {v}"));
        Error::InvalidArguments(format!(
            "`{OPT}` ({version}) could not promote module {module}: {error}"
        ))
    })
}

/// [`emit_module`] for a `slice` answer `session` gave. A slice that found
/// no path is an error, never an empty module.
pub fn emit_slice(
    session: &Session,
    catalog: &Path,
    answer: &QueryResult,
    llvm_link: &Path,
    llvm_bindir: &LlvmBindir,
    out: &Path,
) -> Result<EmittedModule, Error> {
    match (&answer.query, &answer.results) {
        (Query::Slice { from, to, .. }, QueryResults::Slice(slice)) => {
            if slice.functions.is_empty() {
                return Err(Error::InvalidArguments(format!(
                    "no path from {from} to {to}; nothing to emit"
                )));
            }
            emit_module(
                session,
                catalog,
                &slice.functions,
                llvm_link,
                llvm_bindir,
                out,
            )
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
    for function in session.functions() {
        if function.linkage == Linkage::Internal
            && let Some(plan) = plans.get_mut(&function.id.module_id)
        {
            plan.internal.insert(function.id.symbol.clone());
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
    /// Every function the module defines with internal or private linkage,
    /// as the facts name it.
    internal: BTreeSet<String>,
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
/// optional value, the one-letter type, and the name, which is the rest of
/// the line: an Objective-C method's holds spaces. Anything else, such as an
/// archive member's header, is skipped.
fn symbol_table(listing: &str) -> Vec<(char, String)> {
    let one_letter = |field: &str| {
        let mut letters = field.chars();
        match (letters.next(), letters.next()) {
            (Some(kind), None) => Some(kind),
            _ => None,
        }
    };
    listing
        .lines()
        .filter_map(|line| {
            let (first, rest) = line.trim().split_once(char::is_whitespace)?;
            let (kind, name) = match one_letter(first) {
                Some(kind) => (kind, rest),
                None => {
                    let (letter, name) = rest.trim_start().split_once(char::is_whitespace)?;
                    (one_letter(letter)?, name)
                }
            };
            let name = name.trim_start();
            (!name.is_empty()).then(|| (kind, name.to_string()))
        })
        .collect()
}

/// The prefix the piece's target puts on every IR name, read off its own
/// listing: the one under which every function cut out is listed. `None`
/// when the listing does not decide between them.
fn global_prefix(cut: &BTreeSet<String>, symbols: &BTreeSet<String>) -> Option<&'static str> {
    let mut fitting = GLOBAL_PREFIXES.iter().copied().filter(|prefix| {
        cut.iter()
            .filter(|symbol| !symbol.starts_with(VERBATIM_NAME))
            .all(|symbol| symbols.contains(&format!("{prefix}{symbol}")))
    });
    match (fitting.next(), fitting.next()) {
        (Some(prefix), None) => Some(prefix),
        _ => None,
    }
}

/// How the target may spell the IR name `symbol` in its symbol table: with
/// `prefix`, or, when that is undecided, with each one a target may use.
fn spellings(symbol: &str, prefix: Option<&str>) -> Vec<String> {
    if let Some(verbatim) = symbol.strip_prefix(VERBATIM_NAME) {
        return vec![verbatim.to_string()];
    }
    match prefix {
        Some(prefix) => vec![format!("{prefix}{symbol}")],
        None => GLOBAL_PREFIXES
            .iter()
            .map(|prefix| format!("{prefix}{symbol}"))
            .collect(),
    }
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
    Ok(run_tool_output(tool, arguments, directory, environment)?.stdout)
}

/// [`run_tool`], answering everything the tool wrote.
fn run_tool_output<S: AsRef<OsStr>>(
    tool: &Path,
    arguments: &[S],
    directory: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<Output, Error> {
    let output = execute_llvm_tool_in_for_output(tool, arguments, directory, environment)?;
    if output.status.success() {
        return Ok(output);
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
    use crate::{
        extract::read_module_ir,
        facts::ModuleAnalysis,
        ir::{OpcodeIr, Operand},
        testing::{compiled_session, facts, report},
    };

    /// A length clamped to a capacity.
    const CLAMP_SOURCE: &str =
        "unsigned f(unsigned len) { unsigned cap = 16; return len < cap ? len : cap; }\n";

    fn fid(symbol: &str) -> FunctionId {
        FunctionId {
            module_id: "m".into(),
            symbol: symbol.into(),
        }
    }

    /// A catalog of `source` compiled at `-O0` (so `optnone` stays), opened
    /// as a session with the real LLVM bindir.
    fn clamp_session(scratch: &tempfile::TempDir) -> Session {
        compiled_session(scratch.path(), "clamp.c", CLAMP_SOURCE, &["-O0", "-g"])
    }

    /// A bindir whose `opt` only reports `major`.
    fn stub_bindir(scratch: &Path, major: u32) -> LlvmBindir {
        let opt = scratch.join(OPT);
        rllvm_testkit::write_executable(
            &opt,
            &format!("#!/bin/sh\necho 'LLVM version {major}.0.0'\n"),
        );
        LlvmBindir::configured(scratch)
    }

    fn session_with_producer(producer: &str) -> Session {
        let mut program = facts(Vec::new(), Vec::new());
        let mut module = report("m", ModuleAnalysis::Analyzed);
        module.producers = vec![producer.into()];
        program.modules = vec![module];
        Session::new(program, Vec::new())
    }

    #[test]
    fn promote_anchor_module_promotes_stack_slots() {
        let scratch = tempfile::tempdir().unwrap();
        let session = clamp_session(&scratch);

        // The module is genuinely `-O0`: unpromoted, the value lives in slots.
        let source = std::fs::read(scratch.path().join("clamp.bc")).unwrap();
        let before = read_module_ir(&source).unwrap();
        let before = before
            .functions
            .iter()
            .find(|f| f.id.symbol == "f")
            .unwrap();
        assert!(before.values.iter().any(|v| v.opcode == OpcodeIr::Load));

        let bytes = promote_anchor_module(&session, &fid("f")).unwrap().bytes;
        let ir = read_module_ir(&bytes).unwrap();
        let f = ir.functions.iter().find(|f| f.id.symbol == "f").unwrap();
        assert!(
            f.values.iter().all(|v| v.opcode != OpcodeIr::Load),
            "mem2reg removes the loads of the `-O0` slots"
        );
        let param = f.params[0];
        assert!(
            f.values.iter().any(|v| v.opcode == OpcodeIr::Phi
                && v.operands.contains(&Operand::Value(param))
                && v.operands
                    .iter()
                    .any(|o| matches!(o, Operand::Const(c) if c.value == "16"))),
            "the clamp merges the parameter and the constant"
        );
        assert!(f.values.iter().any(|v| {
            v.opcode == OpcodeIr::ICmp
                && v.guard
                    .as_ref()
                    .is_some_and(|g| g.bound.as_deref() == Some("16"))
        }));
    }

    #[test]
    fn a_session_without_a_catalog_or_bindir_says_which_is_missing() {
        let session = session_with_producer("clang version 20.1.0");
        let error = promote_anchor_module(&session, &fid("f")).unwrap_err();
        assert!(error.to_string().contains("catalog file"), "{error}");

        let scratch = tempfile::tempdir().unwrap();
        let catalog = clamp_session(&scratch)
            .catalog_path()
            .unwrap()
            .to_path_buf();
        let fresh = crate::open(&catalog).unwrap();
        let error = promote_anchor_module(&fresh, &fid("f")).unwrap_err();
        assert!(error.to_string().contains("no LLVM bindir"), "{error}");

        let mut failed = crate::open(&catalog).unwrap();
        failed.set_llvm_bindir(Err("llvm-config not found".into()));
        let error = promote_anchor_module(&failed, &fid("f")).unwrap_err();
        assert!(
            error.to_string().contains("llvm-config not found"),
            "{error}"
        );
    }

    #[test]
    fn an_opt_older_than_the_producer_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session_with_producer("clang version 20.1.0 (https://example)");
        let error = check_opt_llvm(&stub_bindir(scratch.path(), 12), &session, "m")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("LLVM 12") && error.contains("LLVM 20"),
            "{error}"
        );
        check_opt_llvm(&stub_bindir(scratch.path(), 20), &session, "m").unwrap();
    }

    #[test]
    fn an_opt_newer_than_the_linked_llvm_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session_with_producer("clang version 20.1.0");
        let linked = linked_major().unwrap();
        let error = check_opt_llvm(&stub_bindir(scratch.path(), linked + 1), &session, "m")
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("LLVM {}", linked + 1)), "{error}");
        assert!(error.contains(&format!("LLVM {linked}")), "{error}");
    }

    /// What a rustc module relies on, its producer naming no LLVM release:
    /// `opt`'s own reader failing is reported with `opt`'s version and the
    /// module, not as a bare tool failure.
    #[test]
    fn a_failing_opt_is_named_with_its_llvm_version_and_the_module() {
        let scratch = tempfile::tempdir().unwrap();
        let mut session = clamp_session(&scratch);
        let bindir = scratch.path().join("bin");
        std::fs::create_dir(&bindir).unwrap();
        let major = linked_major().unwrap();
        let opt = bindir.join(OPT);
        rllvm_testkit::write_executable(
            &opt,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'LLVM version {major}.0.0'; exit 0; }}\necho 'error: cannot read' >&2\nexit 1\n"
            ),
        );
        session.set_llvm_bindir(Ok(LlvmBindir::configured(&bindir)));

        let error = promote_anchor_module(&session, &fid("f"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("(LLVM {major})")) && error.contains("module m"),
            "{error}"
        );
    }

    #[test]
    fn a_producer_without_an_llvm_major_is_left_to_opts_reader() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session_with_producer("rustc version 1.90.0 (abc 2026-01-01)");
        check_opt_llvm(&stub_bindir(scratch.path(), 12), &session, "m").unwrap();
    }

    /// An Objective-C method's name holds a space, and an undefined symbol
    /// has no address.
    #[test]
    fn a_name_keeps_its_spaces_and_an_address_is_optional() {
        let listing = "---------------- t -[Foo bar:baz:]\nU -[Foo other]\n   U _leaf  \n";
        assert_eq!(
            symbol_table(listing),
            [
                ('t', "-[Foo bar:baz:]".to_string()),
                ('U', "-[Foo other]".to_string()),
                ('U', "_leaf".to_string()),
            ]
        );
    }

    #[test]
    fn the_prefix_is_read_off_the_listing() {
        let cut = BTreeSet::from(["fa".to_string(), "\u{1}-[Foo bar:]".to_string()]);
        let macho = BTreeSet::from(["_fa".to_string(), "-[Foo bar:]".to_string()]);
        assert_eq!(global_prefix(&cut, &macho), Some("_"));
        assert_eq!(spellings("pf", Some("_")), ["_pf"]);
        let elf = BTreeSet::from(["fa".to_string(), "-[Foo bar:]".to_string()]);
        assert_eq!(global_prefix(&cut, &elf), Some(""));
        assert_eq!(spellings("\u{1}-[Foo baz]", Some("_")), ["-[Foo baz]"]);

        // Both spellings listed, or neither: undecided, so both are kept.
        let both = BTreeSet::from(["fa".to_string(), "_fa".to_string()]);
        assert_eq!(global_prefix(&cut, &both), None);
        assert_eq!(spellings("pf", None), ["pf", "_pf"]);
    }

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
