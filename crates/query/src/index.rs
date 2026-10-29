//! Indexes over extracted facts, and the graph walks that answer
//! reachability.
//!
//! Every walk here distinguishes exactly three kinds of call-site evidence:
//! a `CallTarget::Direct` edge, a `CallTarget::Indirect` edge CVP bounded to
//! a known set of targets (traversed, one edge per bound member), and a
//! `CallTarget::Indirect` edge with no bound (uncertainty, never traversed).
//! The heuristic address-taken inventory recorded in `ProgramFacts::uses` is
//! not an edge source at all and is never consulted here.
//!
//! An overlay of agent-proposed edges is walked only when a caller passes an
//! [`OverlayView`], and every step through one is a [`PathStep::Agent`]:
//! a hypothesis about an unbounded site, never proof.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use serde::Serialize;

use rllvm_core::{
    catalog::{CatalogOrigin, CatalogScope},
    config::LlvmBindir,
    utils::same_file,
};

use crate::{
    CacheReport,
    overlay::{Confidence, EdgeKey, OverlayView, Verdict},
};

use super::{
    bind::{BindingStatus, SymbolBinding},
    extract::demangle,
    facts::{
        CallSiteFact, CallSiteId, CallTarget, FieldFlow, FieldRef, FunctionFact, FunctionId,
        ModuleReport, ProgramFacts, ReturnedAddress, Slot, SlotFlow, SourceLocation, UseFact,
    },
};

/// How a queried name reached the symbols it named.
///
/// Reported on every answer that took a name, because the three are not
/// equally certain: the first two identify one function each, while `Fuzzy`
/// can gather unrelated functions that merely share an identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameMatch {
    /// The name is a mangled symbol, matched exactly. A C name lands here
    /// too: it is its own symbol.
    Mangled,
    /// The name is a full demangled reading, matched exactly --
    /// `int twice<int>(int)`.
    Demangled,
    /// The name occurs as a whole identifier inside one or more demangled
    /// readings -- `twice` finding `int twice<int>(int)`. A convenience for
    /// typing, and the only tier that can answer for several unrelated
    /// functions at once.
    Fuzzy,
}

/// The symbols a queried name resolved to, and which tier found them.
#[derive(Clone, Debug)]
pub struct NameResolution {
    pub matched: NameMatch,
    pub ids: Vec<FunctionId>,
}

/// Whether `needle` occurs in `haystack` bounded by non-identifier
/// characters, so `twice` is found in `int twice<int>(int)` and in
/// `void wrap::twice()` but not in `twice_helper` or `mytwice`.
///
/// Deliberately not a parse of the demangled declaration: deriving a "base
/// name" by stripping return type, template arguments and parameters means
/// parsing C++ with string operations, which breaks on operator overloads
/// (`operator<<`) and nested templates in ways that are silent.
fn contains_identifier(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let boundary = |character: char| !(character.is_alphanumeric() || character == '_');
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        if haystack[..start].chars().next_back().is_none_or(boundary)
            && haystack[end..].chars().next().is_none_or(boundary)
        {
            return true;
        }
        // Past this occurrence's first character, on a character boundary.
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Direction of a transitive closure walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Functions that reach the named function.
    In,
    /// Functions the named function reaches.
    Out,
}

/// One step on a path `Session::reach` returns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathStep {
    /// A direct call.
    Call(CallSiteId),
    /// An indirect call CVP bounded to a known set of targets. The path
    /// holds only if the call actually takes `chosen`; `bound` carries every
    /// alternative so a reader is not handed one target as though it were
    /// certain.
    BoundedIndirect {
        site: CallSiteId,
        chosen: FunctionId,
        bound: Vec<FunctionId>,
    },
    /// A declaration resolved to its one visible definition.
    Binding(SymbolBinding),
    /// An alias followed to the function it stands for. Certain: the alias
    /// has no body of its own, so whatever calls it runs `target`.
    Alias {
        alias: FunctionId,
        target: FunctionId,
    },
    /// An edge an agent proposed for an unresolved indirect call. A
    /// hypothesis: the path holds only if the call can take `chosen`.
    Agent {
        site: CallSiteId,
        chosen: FunctionId,
        /// The call's source location, so a reader can check the claim.
        #[serde(skip_serializing_if = "Option::is_none")]
        location: Option<SourceLocation>,
        key: Box<EdgeKey>,
        confidence: Confidence,
        /// Why the agent claimed the edge, quoted from the record.
        provenance: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict: Option<Verdict>,
    },
}

impl PathStep {
    /// The call site the step goes through; `None` for a binding or an
    /// alias, which make no call.
    pub(crate) fn site(&self) -> Option<&CallSiteId> {
        match self {
            PathStep::Call(site)
            | PathStep::BoundedIndirect { site, .. }
            | PathStep::Agent { site, .. } => Some(site),
            PathStep::Binding(_) | PathStep::Alias { .. } => None,
        }
    }
}

/// The result of a `Session::reach` query.
#[derive(Debug)]
pub struct ReachResult {
    pub path: Option<Vec<PathStep>>,
    /// Ambiguous bindings the search reached but could not resolve, recorded
    /// rather than guessed through. One entry per symbol, however many
    /// declarations of it the walk passed through.
    pub frontier: Vec<SymbolBinding>,
}

/// The result of a `Session::slice_with` query: the part of the call graph
/// that lies on some path from one function to another.
#[derive(Debug, Serialize)]
pub struct SliceResult {
    /// Every function on some path from `from` to `to`: the forward closure
    /// of `from` (plus `from`) intersected with the backward closure of `to`
    /// (plus `to`'s definitions). Sorted by id. Empty when no path exists.
    pub functions: Vec<FunctionId>,
    /// Every walked edge between two members, with its labeled step.
    pub edges: Vec<SliceEdge>,
}

/// One walked edge between two members of a slice.
#[derive(Debug, Serialize)]
pub struct SliceEdge {
    pub from: FunctionId,
    pub to: FunctionId,
    pub step: PathStep,
}

/// A file a query names, matched against the files the debug info records.
pub(crate) struct NamedFile {
    as_written: PathBuf,
    /// `as_written` made absolute against the working directory; `None` when that fails.
    absolute: Option<PathBuf>,
    /// The recorded directories, by module id and then recorded file, of
    /// every recorded location that resolves to the same file as `absolute`.
    /// Keyed so a lookup is constant-time: `at` asks once per spelling.
    learned: HashMap<String, HashMap<PathBuf, HashSet<Option<PathBuf>>>>,
    /// `same_file` answers by resolved path. Where a path cannot be resolved
    /// it answers only for the identical spelling, never by a failed check.
    same: RefCell<HashMap<PathBuf, bool>>,
}

impl NamedFile {
    /// Whether a location or declaration recorded in `module_id` as `file`
    /// (relative to `directory`, when recorded) is this file.
    ///
    /// A spelling learned from the facts matches only with the directory it
    /// was recorded under, so two files one module spells alike are never
    /// confused. A file no fact records is still named through the directory
    /// it records. A relative file with no recorded directory is never
    /// guessed at.
    pub(crate) fn names(&self, module_id: &str, file: &Path, directory: Option<&Path>) -> bool {
        file == self.as_written
            || self
                .learned
                .get(module_id)
                .and_then(|files| files.get(file))
                .is_some_and(|recorded| {
                    recorded
                        .iter()
                        .any(|recorded| recorded.as_deref() == directory)
                })
            || directory.is_some_and(|directory| self.resolves_here(&directory.join(file)))
    }

    /// Whether the recorded path `resolved` is this file, as the disk
    /// resolves both: one check per path, since each touches the disk. Only
    /// an absolute path is compared, and where either cannot be resolved
    /// [`same_file`] matches only the identical spelling, so a catalog moved
    /// after capture never matches by a check that failed.
    fn resolves_here(&self, resolved: &Path) -> bool {
        let Some(absolute) = &self.absolute else {
            return false;
        };
        resolved.is_absolute()
            && *self
                .same
                .borrow_mut()
                .entry(resolved.to_path_buf())
                .or_insert_with(|| same_file(resolved, absolute))
    }
}

/// Indexes over one program's captured facts, joined with resolved symbol
/// bindings. `reach` and `closure` walk only resolved edges: direct calls,
/// and indirect calls CVP bounded to a bound.
pub struct Session {
    facts: ProgramFacts,
    bindings: Vec<SymbolBinding>,
    by_name: HashMap<String, Vec<FunctionId>>,
    function_index: HashMap<FunctionId, usize>,
    /// Call sites owned by a function, i.e. the calls it makes.
    callees_by_function: HashMap<FunctionId, Vec<usize>>,
    /// Call sites that resolve to a function, directly or as a member of an
    /// LLVM-bounded indirect call's bound, i.e. the calls made to it.
    callers_by_function: HashMap<FunctionId, Vec<usize>>,
    /// Every call site by identity, for an overlay record naming one.
    site_index: HashMap<CallSiteId, usize>,
    /// Unresolved indirect sites (no LLVM bound) keyed by the field they
    /// dispatch through: the sites an overlay field edge attaches to.
    unresolved_by_field: HashMap<FieldRef, Vec<usize>>,
    /// Keyed by the canonical slot, so a site reading a parameter through a
    /// declaration meets the definition's parameter.
    unresolved_by_slot: HashMap<Slot, Vec<usize>>,
    /// Bindings keyed by symbol, for resolving a declaration forward to its
    /// candidates.
    bindings_by_symbol: HashMap<String, Vec<usize>>,
    /// `Unique` bindings keyed by their one candidate, for reverse (`In`)
    /// closure walks back through the declaration that resolved to it.
    bindings_by_candidate: HashMap<FunctionId, Vec<usize>>,
    /// Aliases keyed by the function they stand for, for walking back from a
    /// target to the names it is called by.
    aliases_by_target: HashMap<FunctionId, Vec<FunctionId>>,
    /// Functions mapped to a source file and line. Read by `functions_at`.
    by_file_line: HashMap<(PathBuf, u32), Vec<FunctionId>>,
    /// The C++ reading of every symbol that has one, keyed by the mangled
    /// name. Built once here rather than per query: a name demangles the same
    /// way every time, the fuzzy tier scans all of them, and the envelope's
    /// `symbols` table quotes from it. Sorted, so a fuzzy answer comes back
    /// in the same order every run.
    demangled: BTreeMap<String, String>,
    /// Function ids keyed by their exact demangled reading.
    by_demangled: HashMap<String, Vec<FunctionId>>,
    /// How the facts cache served this session's load; `None` when the
    /// session was opened without one.
    cache_report: Option<CacheReport>,
    /// The catalog file this session was opened from; `None` for one built
    /// from facts or from a catalog held in memory.
    catalog_path: Option<PathBuf>,
    /// The LLVM bindir the tools that transform a module run from, or why
    /// there is none; `None` until a caller says.
    llvm_bindir: Option<Result<LlvmBindir, String>>,
}

impl Session {
    pub fn new(facts: ProgramFacts, bindings: Vec<SymbolBinding>) -> Session {
        let mut by_name: HashMap<String, Vec<FunctionId>> = HashMap::new();
        let mut function_index: HashMap<FunctionId, usize> = HashMap::new();
        let mut by_file_line: HashMap<(PathBuf, u32), Vec<FunctionId>> = HashMap::new();
        let mut aliases_by_target: HashMap<FunctionId, Vec<FunctionId>> = HashMap::new();
        for (idx, function) in facts.functions.iter().enumerate() {
            if let Some(target) = &function.alias_of {
                aliases_by_target
                    .entry(target.clone())
                    .or_default()
                    .push(function.id.clone());
            }
            by_name
                .entry(function.id.symbol.clone())
                .or_default()
                .push(function.id.clone());
            function_index.insert(function.id.clone(), idx);
            for (file, line) in &function.mapped_lines {
                by_file_line
                    .entry((file.clone(), *line))
                    .or_default()
                    .push(function.id.clone());
            }
        }

        let mut callees_by_function: HashMap<FunctionId, Vec<usize>> = HashMap::new();
        let mut callers_by_function: HashMap<FunctionId, Vec<usize>> = HashMap::new();
        let mut site_index: HashMap<CallSiteId, usize> = HashMap::new();
        let mut unresolved_by_field: HashMap<FieldRef, Vec<usize>> = HashMap::new();
        for (idx, site) in facts.call_sites.iter().enumerate() {
            site_index.insert(site.id.clone(), idx);
            callees_by_function
                .entry(site.id.function.clone())
                .or_default()
                .push(idx);
            match &site.target {
                CallTarget::Direct { callee } => {
                    callers_by_function
                        .entry(callee.clone())
                        .or_default()
                        .push(idx);
                }
                CallTarget::Indirect {
                    llvm_target_bound: Some(bound),
                    ..
                } => {
                    for callee in bound {
                        callers_by_function
                            .entry(callee.clone())
                            .or_default()
                            .push(idx);
                    }
                }
                CallTarget::Indirect {
                    llvm_target_bound: None,
                    via_field,
                    ..
                } => {
                    if let Some(evidence) = via_field {
                        unresolved_by_field
                            .entry(evidence.field.clone())
                            .or_default()
                            .push(idx);
                    }
                }
                CallTarget::Intrinsic { .. } | CallTarget::InlineAsm => {}
            }
        }

        let mut bindings_by_symbol: HashMap<String, Vec<usize>> = HashMap::new();
        let mut bindings_by_candidate: HashMap<FunctionId, Vec<usize>> = HashMap::new();
        for (idx, binding) in bindings.iter().enumerate() {
            bindings_by_symbol
                .entry(binding.symbol.clone())
                .or_default()
                .push(idx);
            if binding.status == BindingStatus::Unique
                && let Some(candidate) = binding.candidates.first()
            {
                bindings_by_candidate
                    .entry(candidate.function.clone())
                    .or_default()
                    .push(idx);
            }
        }

        // One demangle per distinct symbol, not per function: the reading is
        // a function of the name alone. `demangle` returns early on anything
        // without the Itanium `_Z` marker, so a C program pays nothing here.
        let distinct: BTreeSet<&str> = facts
            .functions
            .iter()
            .map(|function| function.id.symbol.as_str())
            .collect();
        let demangled: BTreeMap<String, String> = distinct
            .into_iter()
            .filter_map(|symbol| demangle(symbol).map(|reading| (symbol.to_string(), reading)))
            .collect();

        let mut by_demangled: HashMap<String, Vec<FunctionId>> = HashMap::new();
        for function in &facts.functions {
            if let Some(reading) = demangled.get(&function.id.symbol) {
                by_demangled
                    .entry(reading.clone())
                    .or_default()
                    .push(function.id.clone());
            }
        }

        let mut session = Session {
            facts,
            bindings,
            by_name,
            function_index,
            callees_by_function,
            callers_by_function,
            site_index,
            unresolved_by_field,
            unresolved_by_slot: HashMap::new(),
            bindings_by_symbol,
            bindings_by_candidate,
            aliases_by_target,
            by_file_line,
            demangled,
            by_demangled,
            cache_report: None,
            catalog_path: None,
            llvm_bindir: None,
        };
        // Canonical slots need the bindings, so they are keyed once the
        // session can resolve them.
        let mut unresolved_by_slot: HashMap<Slot, Vec<usize>> = HashMap::new();
        for (idx, site) in session.facts.call_sites.iter().enumerate() {
            if let CallTarget::Indirect {
                llvm_target_bound: None,
                via_field: None,
                via_slot: Some(slot),
                ..
            } = &site.target
            {
                unresolved_by_slot
                    .entry(session.canonical_slot(slot))
                    .or_default()
                    .push(idx);
            }
        }
        session.unresolved_by_slot = unresolved_by_slot;
        session
    }

    /// Records how the facts cache served the load that built this session.
    pub fn with_cache_report(mut self, report: Option<CacheReport>) -> Session {
        self.cache_report = report;
        self
    }

    /// Records the catalog file this session was opened from.
    pub fn with_catalog_path(mut self, path: &Path) -> Session {
        self.catalog_path = Some(path.to_path_buf());
        self
    }

    pub fn catalog_path(&self) -> Option<&Path> {
        self.catalog_path.as_deref()
    }

    /// Records the LLVM bindir the module-transforming tools run from, or
    /// the message saying why there is none.
    pub fn set_llvm_bindir(&mut self, llvm_bindir: Result<LlvmBindir, String>) {
        self.llvm_bindir = Some(llvm_bindir);
    }

    pub fn llvm_bindir(&self) -> Option<&Result<LlvmBindir, String>> {
        self.llvm_bindir.as_ref()
    }

    pub fn cache_report(&self) -> Option<&CacheReport> {
        self.cache_report.as_ref()
    }

    /// The symbols a queried name names, and how it got there.
    ///
    /// Tried in order of certainty, stopping at the first tier that answers:
    /// the mangled symbol, then the full demangled reading, then the
    /// identifier search. The exact tiers come first so a program that
    /// genuinely defines a symbol spelled like someone else's identifier is
    /// never answered fuzzily.
    pub fn resolve(&self, name: &str) -> Option<NameResolution> {
        if let Some(ids) = self.by_name.get(name) {
            return Some(NameResolution {
                matched: NameMatch::Mangled,
                ids: ids.clone(),
            });
        }
        if let Some(ids) = self.by_demangled.get(name) {
            return Some(NameResolution {
                matched: NameMatch::Demangled,
                ids: ids.clone(),
            });
        }
        let ids: Vec<FunctionId> = self
            .demangled
            .iter()
            .filter(|(_, reading)| contains_identifier(reading, name))
            .flat_map(|(symbol, _)| self.by_name.get(symbol).into_iter().flatten())
            .cloned()
            .collect();
        (!ids.is_empty()).then_some(NameResolution {
            matched: NameMatch::Fuzzy,
            ids,
        })
    }

    /// The C++ reading of one symbol, for the envelope's `symbols` table.
    /// Not public API: internal plumbing for `crate::run`.
    pub(crate) fn demangled(&self, symbol: &str) -> Option<&str> {
        self.demangled.get(symbol).map(String::as_str)
    }

    /// Call sites that resolve to the named function: a direct call, or an
    /// indirect call CVP bounded to a set that includes it. A call to one of
    /// its aliases runs it too, so those sites count, including calls from
    /// other modules whose declaration binds uniquely to the alias.
    pub fn callers(&self, name: &str) -> Vec<CallSiteFact> {
        let mut targets = Vec::new();
        for id in self.ids_by_name(name) {
            for alias in self.aliases_by_target.get(&id).into_iter().flatten() {
                targets.push(alias.clone());
                for &binding_idx in self.bindings_by_candidate.get(alias).into_iter().flatten() {
                    let binding = &self.bindings[binding_idx];
                    targets.extend(binding.declared_in.iter().map(|module| FunctionId {
                        module_id: module.clone(),
                        symbol: binding.symbol.clone(),
                    }));
                }
            }
            targets.push(id);
        }
        let mut seen = BTreeSet::new();
        targets
            .iter()
            .flat_map(|id| self.callers_by_function.get(id).into_iter().flatten())
            .filter(|&&idx| seen.insert(idx))
            .map(|&idx| self.facts.call_sites[idx].clone())
            .collect()
    }

    /// Call sites the named function makes, resolved or not. An alias has no
    /// body, so its call sites are its target's.
    pub fn callees(&self, name: &str) -> Vec<CallSiteFact> {
        self.ids_by_name(name)
            .iter()
            .map(|id| self.alias_target(id).unwrap_or(id))
            .flat_map(|id| self.callees_by_function.get(id).into_iter().flatten())
            .map(|&idx| self.facts.call_sites[idx].clone())
            .collect()
    }

    /// Functions whose recorded source mapping includes `file:line`, with
    /// `file` matched as [`NamedFile::names`] decides, in function order.
    /// Not public API: internal plumbing for the `at` query, called by
    /// `crate::run`.
    pub(crate) fn functions_at(&self, file: &Path, line: u32) -> Vec<FunctionId> {
        let named = self.named_file(file);
        // A mapped line records its file without a directory. A spelling
        // the module records under several directories is claimed only when
        // the matcher names every one: which of them the line is in is
        // unknown. A directory that only mapped lines use, with no function,
        // call or use fact recorded under it, is unseen here, so the spelling
        // is still claimed under the directories the facts record; closing
        // that needs directories in `mapped_lines`, a facts-cache change.
        let mut directories: HashMap<(&str, &Path), HashSet<Option<&Path>>> = HashMap::new();
        for (module_id, location) in self.recorded_locations() {
            directories
                .entry((module_id, location.file.as_path()))
                .or_default()
                .insert(location.directory.as_deref());
        }
        let mapped = |spelling: &Path| self.by_file_line.get(&(spelling.to_path_buf(), line));
        let mut found: BTreeSet<usize> = mapped(file)
            .into_iter()
            .flatten()
            .map(|id| self.function_index[id])
            .collect();
        for ((module_id, spelling), recorded) in directories {
            if spelling != file
                && recorded
                    .into_iter()
                    .all(|directory| named.names(module_id, spelling, directory))
            {
                found.extend(
                    mapped(spelling)
                        .into_iter()
                        .flatten()
                        .filter(|id| id.module_id == module_id)
                        .map(|id| self.function_index[id]),
                );
            }
        }
        found
            .into_iter()
            .map(|idx| self.facts.functions[idx].id.clone())
            .collect()
    }

    /// Every location a function, call or use fact records, with the module
    /// it is recorded in.
    fn recorded_locations(&self) -> impl Iterator<Item = (&str, &SourceLocation)> {
        let functions = self
            .facts
            .functions
            .iter()
            .map(|function| (&function.id, function.location.as_ref()));
        let sites = self
            .facts
            .call_sites
            .iter()
            .map(|site| (&site.id.function, site.location.as_ref()));
        let uses = self.facts.uses.iter().filter_map(|use_fact| {
            Some((use_fact.in_function.as_ref()?, use_fact.location.as_ref()))
        });
        functions
            .chain(sites)
            .chain(uses)
            .filter_map(|(id, location)| Some((id.module_id.as_str(), location?)))
    }

    /// The matcher for `file` as a query names it: the spelling as written,
    /// and each `(module, file, directory)` the facts record for the same
    /// file -- named relative to the working directory, absolute, or through
    /// a symlink the compiler resolved in the file but not in its recorded
    /// directory.
    pub(crate) fn named_file(&self, file: &Path) -> NamedFile {
        let absolute = std::path::absolute(file).ok();
        let named = NamedFile {
            as_written: file.to_path_buf(),
            absolute,
            learned: HashMap::new(),
            same: RefCell::new(HashMap::new()),
        };
        let mut learned: HashMap<String, HashMap<PathBuf, HashSet<Option<PathBuf>>>> =
            HashMap::new();
        for (module_id, location) in self.recorded_locations() {
            if named.resolves_here(&location.resolved_path()) {
                learned
                    .entry(module_id.to_string())
                    .or_default()
                    .entry(location.file.clone())
                    .or_default()
                    .insert(location.directory.clone());
            }
        }
        NamedFile { learned, ..named }
    }

    /// The heuristic address-taken inventory, verbatim. Never consulted by
    /// `reach`/`closure`; exposed only so an opt-in `indirect-targets`
    /// answer can report it. Not public API: internal plumbing, called by
    /// `crate::run`.
    pub(crate) fn slot_flows(&self) -> &[SlotFlow] {
        &self.facts.slot_flows
    }

    pub(crate) fn field_flows(&self) -> &[FieldFlow] {
        &self.facts.field_flows
    }

    pub(crate) fn returned_addresses(&self) -> &[ReturnedAddress] {
        &self.facts.returned_addresses
    }

    pub(crate) fn uses(&self) -> &[UseFact] {
        &self.facts.uses
    }

    /// Every function in the selected scope. Not public API: internal
    /// plumbing for envelope-level counts such as functions without a
    /// recorded location.
    pub(crate) fn functions(&self) -> &[FunctionFact] {
        &self.facts.functions
    }

    /// One function's fact, by identity. Not public API: internal plumbing
    /// for shaping an answer around a function already named by an edge.
    pub(crate) fn function(&self, id: &FunctionId) -> Option<&FunctionFact> {
        self.function_index
            .get(id)
            .map(|&idx| &self.facts.functions[idx])
    }

    /// Definitions of `name`. Declarations are excluded: a `defs` answer
    /// reports where the symbol is defined, not every place it is merely
    /// declared. Not public API: internal plumbing for `crate::run`.
    pub(crate) fn definitions(&self, name: &str) -> Vec<&FunctionFact> {
        self.ids_by_name(name)
            .into_iter()
            .filter_map(|id| self.function(&id))
            .filter(|function| function.is_definition)
            .collect()
    }

    /// Every call site in the selected scope, resolved or not. Not public
    /// API: internal plumbing for the envelope's uncertainty counts, which
    /// must see every indirect site regardless of which function query ran.
    pub(crate) fn call_sites(&self) -> &[CallSiteFact] {
        &self.facts.call_sites
    }

    /// One call site, by identity. Not public API: internal plumbing for
    /// validating an overlay record that names a site.
    pub(crate) fn call_site(&self, id: &CallSiteId) -> Option<&CallSiteFact> {
        self.site_index
            .get(id)
            .map(|&idx| &self.facts.call_sites[idx])
    }

    /// Unresolved indirect call sites -- no LLVM bound -- that dispatch
    /// through `field`. Not public API: internal plumbing for the overlay,
    /// whose field edges attach to exactly these sites and never to a
    /// bounded one.
    pub(crate) fn unresolved_sites_through(&self, field: &FieldRef) -> Vec<&CallSiteFact> {
        self.unresolved_by_field
            .get(field)
            .into_iter()
            .flatten()
            .map(|&idx| &self.facts.call_sites[idx])
            .collect()
    }

    /// Call sites recorded at `file:line`, in any function, with `file`
    /// matched as [`NamedFile::names`] decides. Not public API: internal
    /// plumbing for `at`, `indirect-targets` and every `--at` site, which key
    /// off a source location rather than a symbol name.
    pub(crate) fn call_sites_at(&self, file: &Path, line: u32) -> Vec<&CallSiteFact> {
        let named = self.named_file(file);
        self.facts
            .call_sites
            .iter()
            .filter(|site| {
                site.location.as_ref().is_some_and(|location| {
                    location.line == line
                        && named.names(
                            &site.id.function.module_id,
                            &location.file,
                            location.directory.as_deref(),
                        )
                })
            })
            .collect()
    }

    /// Per-module analysis reports, quoted from the catalog load and
    /// extraction. Not public API: internal plumbing for the envelope's
    /// `analysis` block, which must count every recorded status.
    pub(crate) fn modules(&self) -> &[ModuleReport] {
        &self.facts.modules
    }

    /// The catalog's selection, quoted verbatim. Not public API: internal
    /// plumbing for the envelope's `scope` block, which must never
    /// recompute what the catalog already selected.
    pub(crate) fn scope(&self) -> &CatalogScope {
        &self.facts.scope
    }

    /// The catalog's origin, quoted verbatim. Not public API: internal
    /// plumbing for the envelope's `provenance` block.
    pub(crate) fn origin(&self) -> &CatalogOrigin {
        &self.facts.origin
    }

    /// Every resolved cross-module binding, `Unbound` included. Not public
    /// API: internal plumbing for `externals` and for the envelope's
    /// program-wide ambiguity count.
    pub(crate) fn bindings(&self) -> &[SymbolBinding] {
        &self.bindings
    }

    /// Breadth-first search over resolved edges: `CallTarget::Direct`,
    /// `CallTarget::Indirect` bounded by CVP, and `Unique` symbol bindings
    /// resolving a declaration. An `Ambiguous` binding halts that branch and
    /// is recorded in `frontier` rather than guessed through. A visited set
    /// guarantees termination on a cycle.
    pub fn reach(&self, from: &str, to: &str) -> ReachResult {
        self.reach_with(from, to, None)
    }

    /// [`Session::reach`], also walking `overlay`'s agent edges when given.
    /// A function's direct edges come before its own agent edges, but the
    /// walk is breadth-first across functions, so it can return a path
    /// through agent edges as short as, or shorter than, a resolved one.
    pub fn reach_with(&self, from: &str, to: &str, overlay: Option<&OverlayView>) -> ReachResult {
        self.reach_to(from, to, overlay).0
    }

    /// [`Session::reach_with`], with the definition the returned path ends
    /// at. Not public API: internal plumbing for the annotations on a path,
    /// whose last function no step names as a caller.
    pub(crate) fn reach_to(
        &self,
        from: &str,
        to: &str,
        overlay: Option<&OverlayView>,
    ) -> (ReachResult, Option<FunctionId>) {
        // A mere declaration is not a reached function: only a definition
        // among the functions named `to` counts as the destination.
        let targets: HashSet<FunctionId> = self
            .ids_by_name(to)
            .into_iter()
            .filter(|id| self.is_definition(id))
            .collect();

        let mut visited: HashSet<FunctionId> = HashSet::new();
        let mut queue: VecDeque<(FunctionId, Vec<PathStep>)> = VecDeque::new();
        let mut frontier: Vec<SymbolBinding> = Vec::new();

        for start in self.ids_by_name(from) {
            if visited.insert(start.clone()) {
                queue.push_back((start, Vec::new()));
            }
        }

        while let Some((current, path)) = queue.pop_front() {
            if targets.contains(&current) {
                let result = ReachResult {
                    path: Some(path),
                    frontier,
                };
                return (result, Some(current));
            }

            let (edges, ambiguous) = self.successors(&current, overlay);
            // One entry per symbol. A symbol declared in several modules is
            // reached once per declaration, and the binding is the same
            // record each time; pushing it repeatedly would report one
            // ambiguous symbol as several.
            if let Some(binding) = ambiguous
                && !frontier.iter().any(|seen| seen.symbol == binding.symbol)
            {
                frontier.push(binding);
            }
            for (next, step) in edges {
                if visited.insert(next.clone()) {
                    let mut next_path = path.clone();
                    next_path.push(step);
                    queue.push_back((next, next_path));
                }
            }
        }

        let result = ReachResult {
            path: None,
            frontier,
        };
        (result, None)
    }

    /// The same walk as `reach`, run to exhaustion in one direction and
    /// collecting every function reached (never including the start set
    /// itself).
    pub fn closure(&self, name: &str, direction: Direction) -> Vec<FunctionId> {
        self.closure_with(name, direction, None)
    }

    /// [`Session::closure`], also walking `overlay`'s agent edges when given.
    pub fn closure_with(
        &self,
        name: &str,
        direction: Direction,
        overlay: Option<&OverlayView>,
    ) -> Vec<FunctionId> {
        self.closure_from(self.ids_by_name(name), direction, overlay)
    }

    /// The functions on some path from `from` to `to`, and the edges among
    /// them: what `reach` would walk, kept whole rather than cut to one path.
    ///
    /// A member is reached from `from` and reaches a definition of `to`.
    /// The backward walk starts from `to`'s definitions alone, as `reach`
    /// accepts only a definition as the destination, so callers of a
    /// declaration that an ambiguous binding leaves unresolved stay out.
    /// `from == to` is the trivial path: its definitions, with no edges.
    pub fn slice_with(&self, from: &str, to: &str, overlay: Option<&OverlayView>) -> SliceResult {
        let destinations: Vec<FunctionId> = self
            .ids_by_name(to)
            .into_iter()
            .filter(|id| self.is_definition(id))
            .collect();
        if from == to {
            let mut functions = destinations;
            functions.sort_unstable();
            return SliceResult {
                functions,
                edges: Vec::new(),
            };
        }

        let starts = self.ids_by_name(from);
        let forward: HashSet<FunctionId> = starts
            .iter()
            .cloned()
            .chain(self.closure_from(starts.clone(), Direction::Out, overlay))
            .collect();
        let backward: HashSet<FunctionId> = destinations
            .iter()
            .cloned()
            .chain(self.closure_from(destinations.clone(), Direction::In, overlay))
            .collect();
        let mut functions: Vec<FunctionId> = forward.intersection(&backward).cloned().collect();
        functions.sort_unstable();

        let members: HashSet<&FunctionId> = functions.iter().collect();
        let mut edges = Vec::new();
        for member in &functions {
            for (next, step) in self.successors(member, overlay).0 {
                if members.contains(&next) {
                    edges.push(SliceEdge {
                        from: member.clone(),
                        to: next,
                        step,
                    });
                }
            }
        }
        SliceResult { functions, edges }
    }

    /// The closure walk from an explicit start set, never including the
    /// start set itself.
    fn closure_from(
        &self,
        starts: Vec<FunctionId>,
        direction: Direction,
        overlay: Option<&OverlayView>,
    ) -> Vec<FunctionId> {
        let mut visited: HashSet<FunctionId> = HashSet::new();
        let mut queue: VecDeque<FunctionId> = VecDeque::new();
        for start in starts {
            if visited.insert(start.clone()) {
                queue.push_back(start);
            }
        }

        let mut reached = Vec::new();
        while let Some(current) = queue.pop_front() {
            let neighbors = match direction {
                Direction::Out => self.successors(&current, overlay).0,
                Direction::In => self.predecessors(&current, overlay),
            };
            for (next, _step) in neighbors {
                if visited.insert(next.clone()) {
                    reached.push(next.clone());
                    queue.push_back(next);
                }
            }
        }
        reached
    }

    fn ids_by_name(&self, name: &str) -> Vec<FunctionId> {
        self.resolve(name)
            .map(|resolution| resolution.ids)
            .unwrap_or_default()
    }

    /// The function `id` stands for when it is an alias.
    fn alias_target(&self, id: &FunctionId) -> Option<&FunctionId> {
        self.function_index
            .get(id)
            .and_then(|&idx| self.facts.functions[idx].alias_of.as_ref())
    }

    fn is_definition(&self, id: &FunctionId) -> bool {
        self.function_index
            .get(id)
            .map(|&idx| self.facts.functions[idx].is_definition)
            .unwrap_or(false)
    }

    /// `slot` with its function bound to the definition it stands for, so the
    /// same parameter or accessor read through a declaration in one module
    /// and defined in another is one slot.
    pub(crate) fn canonical_slot(&self, slot: &Slot) -> Slot {
        let bound = |function: &FunctionId| {
            self.bound_definition(function)
                .unwrap_or_else(|| function.clone())
        };
        match slot {
            Slot::Param { function, index } => Slot::Param {
                function: bound(function),
                index: *index,
            },
            Slot::Returned { function } => Slot::Returned {
                function: bound(function),
            },
            Slot::Global { .. } => slot.clone(),
        }
    }

    /// Unresolved indirect call sites that read their callee from `slot`, in
    /// any spelling of it. Like `unresolved_sites_through`, the sites an
    /// overlay slot edge attaches to.
    pub(crate) fn unresolved_sites_via(&self, slot: &Slot) -> Vec<&CallSiteFact> {
        self.unresolved_by_slot
            .get(&self.canonical_slot(slot))
            .into_iter()
            .flatten()
            .map(|&idx| &self.facts.call_sites[idx])
            .collect()
    }

    /// The definition `id` stands for: itself, or the one definition its
    /// declaration binds to.
    pub(crate) fn bound_definition(&self, id: &FunctionId) -> Option<FunctionId> {
        if self.is_definition(id) {
            return Some(id.clone());
        }
        let binding = self.binding_for_declaration(id)?;
        (binding.status == BindingStatus::Unique)
            .then(|| {
                binding
                    .candidates
                    .first()
                    .map(|candidate| candidate.function.clone())
            })
            .flatten()
    }

    fn binding_for_declaration(&self, id: &FunctionId) -> Option<&SymbolBinding> {
        self.bindings_by_symbol
            .get(&id.symbol)?
            .iter()
            .map(|&idx| &self.bindings[idx])
            .find(|binding| {
                binding
                    .declared_in
                    .iter()
                    .any(|module| module == &id.module_id)
            })
    }

    /// Forward edges out of `id`: a definition's resolved call sites, then
    /// its agent edges from `overlay`, or a declaration's `Unique` binding.
    /// Also returns the binding at `id` when it is `Ambiguous`, so callers
    /// can record it in a `frontier` without treating it as an edge.
    fn successors(
        &self,
        id: &FunctionId,
        overlay: Option<&OverlayView>,
    ) -> (Vec<(FunctionId, PathStep)>, Option<SymbolBinding>) {
        if !self.is_definition(id) {
            return match self.binding_for_declaration(id).cloned() {
                Some(binding) => match binding.status {
                    BindingStatus::Unique => match binding.candidates.first() {
                        Some(candidate) => {
                            let function = candidate.function.clone();
                            (vec![(function, PathStep::Binding(binding))], None)
                        }
                        // `SymbolBinding` is public and `Session::new` does
                        // not validate it: a caller-built `Unique` binding
                        // with no candidate has nothing to resolve to.
                        None => (Vec::new(), None),
                    },
                    BindingStatus::Ambiguous => (Vec::new(), Some(binding)),
                    BindingStatus::Unbound => (Vec::new(), None),
                },
                None => (Vec::new(), None),
            };
        }

        if let Some(target) = self.alias_target(id) {
            let step = PathStep::Alias {
                alias: id.clone(),
                target: target.clone(),
            };
            return (vec![(target.clone(), step)], None);
        }

        let mut edges = Vec::new();
        for &site_idx in self.callees_by_function.get(id).into_iter().flatten() {
            let site = &self.facts.call_sites[site_idx];
            match &site.target {
                CallTarget::Direct { callee } => {
                    edges.push((callee.clone(), PathStep::Call(site.id.clone())));
                }
                CallTarget::Indirect {
                    llvm_target_bound: Some(bound),
                    ..
                } => {
                    for chosen in bound {
                        edges.push((
                            chosen.clone(),
                            PathStep::BoundedIndirect {
                                site: site.id.clone(),
                                chosen: chosen.clone(),
                                bound: bound.clone(),
                            },
                        ));
                    }
                }
                CallTarget::Indirect {
                    llvm_target_bound: None,
                    ..
                }
                | CallTarget::Intrinsic { .. }
                | CallTarget::InlineAsm => {}
            }
        }
        if let Some(overlay) = overlay {
            edges.extend_from_slice(overlay.successors(id));
        }
        (edges, None)
    }

    /// Reverse edges into `id`: call sites resolving to it, and any `Unique`
    /// binding for which it is the one candidate, walked back to the
    /// declaration(s) that resolve to it; then agent edges into it from
    /// `overlay`.
    fn predecessors(
        &self,
        id: &FunctionId,
        overlay: Option<&OverlayView>,
    ) -> Vec<(FunctionId, PathStep)> {
        let mut edges = Vec::new();
        for &site_idx in self.callers_by_function.get(id).into_iter().flatten() {
            let site = &self.facts.call_sites[site_idx];
            let step = match &site.target {
                CallTarget::Direct { .. } => PathStep::Call(site.id.clone()),
                CallTarget::Indirect {
                    llvm_target_bound: Some(bound),
                    ..
                } => PathStep::BoundedIndirect {
                    site: site.id.clone(),
                    chosen: id.clone(),
                    bound: bound.clone(),
                },
                _ => continue,
            };
            edges.push((site.id.function.clone(), step));
        }

        for &binding_idx in self.bindings_by_candidate.get(id).into_iter().flatten() {
            let binding = &self.bindings[binding_idx];
            for module in &binding.declared_in {
                let declaration = FunctionId {
                    module_id: module.clone(),
                    symbol: binding.symbol.clone(),
                };
                edges.push((declaration, PathStep::Binding(binding.clone())));
            }
        }

        for alias in self.aliases_by_target.get(id).into_iter().flatten() {
            let step = PathStep::Alias {
                alias: alias.clone(),
                target: id.clone(),
            };
            edges.push((alias.clone(), step));
        }
        if let Some(overlay) = overlay {
            edges.extend_from_slice(overlay.predecessors(id));
        }
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{facts::Linkage, testing::*};

    #[test]
    fn reach_follows_a_chain_of_direct_calls() {
        let session = session_from(&[("a", "b"), ("b", "c")]);
        let result = session.reach("a", "c");
        assert!(result.path.is_some());
    }

    /// Two modules declare one ambiguous symbol, so the walk reaches the
    /// same binding twice. It must not invent a path through the ambiguity,
    /// and the frontier must report one symbol once: pushing per declaration
    /// reports one problem as several, and every count downstream inherits
    /// the inflation.
    #[test]
    fn one_ambiguous_symbol_reached_twice_is_reported_once() {
        let session = session_with_ambiguous_bindings();
        let result = session.reach("caller", "target");
        assert!(result.path.is_none());
        assert_eq!(
            result.frontier.len(),
            1,
            "one symbol, however many declarations reached it: {:?}",
            result.frontier
        );
        assert_eq!(result.frontier[0].symbol, "target");
        assert_eq!(
            result.frontier[0].declared_in.len(),
            2,
            "the one entry still names both declaring modules"
        );
        assert_eq!(
            result.frontier[0].candidates.len(),
            2,
            "the walk reports the candidates it refused to choose between"
        );
    }

    #[test]
    fn reach_does_not_panic_on_a_unique_binding_built_with_no_candidates() {
        // `SymbolBinding` is public and `Session::new` does not validate it
        // against `bind()`'s invariant that `Unique` implies one candidate.
        let caller = function("c", "caller", true, Linkage::External);
        let declaration = function("c", "target", false, Linkage::External);
        let call = direct_call(&caller, &declaration, 0);
        let binding = SymbolBinding {
            symbol: "target".into(),
            declared_in: vec!["c".into()],
            candidates: Vec::new(),
            status: BindingStatus::Unique,
        };
        let session = Session::new(facts(vec![caller, declaration], vec![call]), vec![binding]);
        assert!(session.reach("caller", "target").path.is_none());
    }

    #[test]
    fn reach_uses_an_llvm_bounded_indirect_edge() {
        // The only route from `a` to `target` runs through an indirect call
        // CVP bounded to {target, other}. Without bounded edges this
        // returns None. The bound has two members so an implementation that
        // collapsed it to just the taken target (`bound: vec![chosen]`)
        // fails this too, not only one that drops the edge outright.
        let session = session_with_bounded_indirect();
        let result = session.reach("a", "target");
        let path = result
            .path
            .expect("a bounded indirect edge is a resolved edge");
        assert!(matches!(
            path.iter().find(|step| matches!(step, PathStep::BoundedIndirect { .. })),
            Some(PathStep::BoundedIndirect { bound, chosen, .. })
                if chosen.symbol == "target"
                    && bound.len() == 2
                    && bound.contains(chosen)
                    && bound.iter().any(|f| f.symbol == "other")
        ));
    }

    #[test]
    fn reach_ignores_an_unbounded_indirect_edge() {
        let session = session_with_indirect_gap();
        assert!(session.reach("a", "c").path.is_none());
    }

    #[test]
    fn reach_never_traverses_the_heuristic_inventory() {
        // `add`'s address is taken, so it is in the inventory, but nothing
        // bounds the call. The inventory must not become an edge.
        let session = session_with_address_taken_function();
        assert!(session.reach("caller", "add").path.is_none());
    }

    #[test]
    fn reach_terminates_on_a_cycle() {
        let session = session_from(&[("a", "b"), ("b", "a")]);
        let result = session.reach("a", "absent");
        assert!(result.path.is_none());
    }

    #[test]
    fn closure_runs_in_both_directions() {
        let session = session_from(&[("a", "b"), ("b", "c")]);
        assert_eq!(session.closure("a", Direction::Out).len(), 2);
        assert_eq!(session.closure("c", Direction::In).len(), 2);
    }

    fn symbols_of(ids: &[FunctionId]) -> Vec<&str> {
        ids.iter().map(|id| id.symbol.as_str()).collect()
    }

    /// `e` is reached from `a` but reaches nothing; `f` reaches `d` but is
    /// not reached from `a`. Neither is on a path from `a` to `d`.
    #[test]
    fn a_slice_holds_exactly_the_functions_on_some_path() {
        let session = session_from(&[
            ("a", "b"),
            ("b", "d"),
            ("a", "c"),
            ("c", "d"),
            ("a", "e"),
            ("f", "d"),
        ]);
        let slice = session.slice_with("a", "d", None);
        assert_eq!(symbols_of(&slice.functions), ["a", "b", "c", "d"]);
        let mut edges: Vec<(&str, &str)> = slice
            .edges
            .iter()
            .map(|edge| (edge.from.symbol.as_str(), edge.to.symbol.as_str()))
            .collect();
        edges.sort_unstable();
        assert_eq!(edges, [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]);
        assert!(
            slice
                .edges
                .iter()
                .all(|edge| matches!(edge.step, PathStep::Call(_))),
            "{:?}",
            slice.edges
        );
    }

    #[test]
    fn no_path_slices_to_nothing() {
        let session = session_from(&[("a", "b"), ("c", "d")]);
        let slice = session.slice_with("a", "d", None);
        assert!(slice.functions.is_empty());
        assert!(slice.edges.is_empty());

        let gap = session_with_indirect_gap();
        assert!(
            gap.slice_with("a", "c", None).functions.is_empty(),
            "an unbounded indirect call is not an edge"
        );
    }

    /// The trivial path: the function itself, with no edges, even when a
    /// cycle runs back through it.
    #[test]
    fn a_slice_from_a_function_to_itself_is_its_definition() {
        let session = session_from(&[("a", "b"), ("b", "a")]);
        let slice = session.slice_with("a", "a", None);
        assert_eq!(symbols_of(&slice.functions), ["a"]);
        assert!(slice.edges.is_empty());
    }

    /// `caller` reaches the definition of `target` through its declaration,
    /// so the declaration is on the path too, joined by the binding.
    #[test]
    fn a_slice_keeps_the_declaration_a_binding_resolves() {
        let caller = function("c", "caller", true, Linkage::External);
        let declaration = function("c", "target", false, Linkage::External);
        let definition = function("t", "target", true, Linkage::External);
        let call = direct_call(&caller, &declaration, 0);
        let binding = SymbolBinding {
            symbol: "target".into(),
            declared_in: vec!["c".into()],
            candidates: vec![crate::bind::BindingCandidate {
                function: definition.id.clone(),
                configuration_id: None,
            }],
            status: BindingStatus::Unique,
        };
        let session = Session::new(
            facts(vec![caller, declaration, definition], vec![call]),
            vec![binding],
        );
        let slice = session.slice_with("caller", "target", None);
        let members: Vec<(&str, &str)> = slice
            .functions
            .iter()
            .map(|id| (id.module_id.as_str(), id.symbol.as_str()))
            .collect();
        assert_eq!(members, [("c", "caller"), ("c", "target"), ("t", "target")]);
        assert!(
            slice
                .edges
                .iter()
                .any(|edge| matches!(edge.step, PathStep::Binding(_))
                    && edge.from.module_id == "c"
                    && edge.to.module_id == "t"),
            "{:?}",
            slice.edges
        );
    }

    #[test]
    fn functions_at_finds_only_the_mapped_line() {
        let session = session_from_source_lines(&[("t.c", 2)]);

        let found = session.functions_at(Path::new("t.c"), 2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].symbol, "only");
        assert!(session.functions_at(Path::new("t.c"), 99).is_empty());
    }

    /// `file:line` recorded as `file` relative to `directory`.
    fn recorded_in(directory: &Path, file: &str, line: u32) -> Option<SourceLocation> {
        Some(SourceLocation {
            directory: Some(directory.to_path_buf()),
            ..source_location(file, line)
        })
    }

    /// Writes `file` under a new directory `name` in `scratch`.
    fn header_in(scratch: &tempfile::TempDir, name: &str, file: &str) -> PathBuf {
        let directory = scratch.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(file), "").unwrap();
        directory
    }

    /// Two headers one module spells alike, `a.h`, in different
    /// directories: the spelling learned for one names only that one.
    #[test]
    fn a_learned_spelling_matches_only_with_its_recorded_directory() {
        let scratch = tempfile::tempdir().unwrap();
        let (one, two) = (
            header_in(&scratch, "dir1", "a.h"),
            header_in(&scratch, "dir2", "a.h"),
        );
        let caller = function("m", "caller", true, Linkage::External);
        let sites = vec![
            indirect_call(&caller, 0, recorded_in(&one, "a.h", 3), None),
            indirect_call(&caller, 1, recorded_in(&two, "a.h", 3), None),
        ];
        let session = Session::new(facts(vec![caller], sites), Vec::new());

        let found: Vec<u32> = session
            .call_sites_at(&one.join("a.h"), 3)
            .iter()
            .map(|site| site.id.instruction_index)
            .collect();
        assert_eq!(found, [0], "only the site in dir1/a.h");
    }

    /// A mapped line records its file without a directory: through a
    /// spelling its module records under one directory it is claimed, but
    /// not through one the module records under two.
    #[test]
    fn a_mapped_line_is_claimed_only_through_an_unambiguous_spelling() {
        let scratch = tempfile::tempdir().unwrap();
        let (one, two) = (
            header_in(&scratch, "dir1", "a.h"),
            header_in(&scratch, "dir2", "a.h"),
        );
        let mut mapped = function("m", "mapped", true, Linkage::Internal);
        mapped.mapped_lines = [(PathBuf::from("a.h"), 3)].into();
        let mut first = function("m", "first", true, Linkage::Internal);
        first.location = recorded_in(&one, "a.h", 1);
        let mut second = function("m", "second", true, Linkage::Internal);
        second.location = recorded_in(&two, "a.h", 1);
        let at = |functions: Vec<FunctionFact>| {
            Session::new(facts(functions, Vec::new()), Vec::new()).functions_at(&one.join("a.h"), 3)
        };

        let found = at(vec![mapped.clone(), first.clone()]);
        assert_eq!(symbols_of(&found), ["mapped"]);
        assert!(at(vec![mapped, first, second]).is_empty());
    }

    /// A file no fact records is still named through a directory a
    /// declaration records, by any path to it.
    #[test]
    fn a_recorded_directory_names_a_file_no_fact_spells() {
        let scratch = tempfile::tempdir().unwrap();
        let real = header_in(&scratch, "real", "h.h");
        let other = header_in(&scratch, "other", "h.h");
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let session = Session::new(facts(Vec::new(), Vec::new()), Vec::new());

        let named = session.named_file(&link.join("h.h"));
        assert!(named.names("m", Path::new("h.h"), Some(&real)));
        assert!(!named.names("m", Path::new("h.h"), Some(&other)));
        assert!(
            !named.names("m", Path::new("h.h"), None),
            "a relative file with no directory is never guessed at"
        );
    }

    /// A catalog moved after capture: its recorded directory is gone, so a
    /// file matches only as spelled, never by a disk check that failed.
    #[test]
    fn a_recorded_directory_missing_from_disk_matches_only_by_spelling() {
        let scratch = tempfile::tempdir().unwrap();
        let real = header_in(&scratch, "real", "h.h");
        let moved = scratch.path().join("moved");
        let caller = function("m", "caller", true, Linkage::External);
        let sites = vec![indirect_call(
            &caller,
            0,
            recorded_in(&moved, "h.h", 2),
            None,
        )];
        let session = Session::new(facts(vec![caller], sites), Vec::new());

        let named = session.named_file(&real.join("h.h"));
        assert!(!named.names("m", Path::new("h.h"), Some(&moved)));
        assert!(session.call_sites_at(&real.join("h.h"), 2).is_empty());
        let gone = session.named_file(&scratch.path().join("gone/h.h"));
        assert!(!gone.names("m", Path::new("h.h"), Some(&moved)));
        assert!(
            session
                .named_file(&moved.join("h.h"))
                .names("m", Path::new("h.h"), Some(&moved)),
            "the exact recorded path still names the file"
        );

        assert!(
            session
                .named_file(Path::new("h.h"))
                .names("m", Path::new("h.h"), Some(&moved))
        );
        assert_eq!(session.call_sites_at(Path::new("h.h"), 2).len(), 1);
    }

    #[test]
    fn a_name_resolves_by_mangled_symbol_then_demangled_reading_then_identifier() {
        let session = session_with_cxx_symbols();

        let mangled = session
            .resolve("_Z5twiceIiET_S0_")
            .expect("the symbol itself");
        assert_eq!(mangled.matched, NameMatch::Mangled);
        assert_eq!(mangled.ids.len(), 1);

        let demangled = session
            .resolve("int twice<int>(int)")
            .expect("the full reading");
        assert_eq!(demangled.matched, NameMatch::Demangled);
        assert_eq!(demangled.ids, mangled.ids, "both name one function");

        // A C name is its own symbol, so it lands in the exact tier.
        assert_eq!(
            session.resolve("main").map(|r| r.matched),
            Some(NameMatch::Mangled)
        );

        assert!(session.resolve("absent").is_none());
    }

    #[test]
    fn the_fuzzy_tier_finds_every_instantiation_and_stops_at_identifier_edges() {
        let session = session_with_cxx_symbols();
        let fuzzy = session.resolve("twice").expect("an identifier search");
        assert_eq!(fuzzy.matched, NameMatch::Fuzzy);

        let mut found: Vec<&str> = fuzzy.ids.iter().map(|id| id.symbol.as_str()).collect();
        found.sort_unstable();
        assert_eq!(
            found,
            ["_Z5twiceIdET_S0_", "_Z5twiceIiET_S0_", "_ZN2ns5twiceEv"],
            "both instantiations and the namespaced one, but never `twice_helper`"
        );
    }

    /// The exact tiers run first, so a program that really defines a symbol
    /// spelled like a common identifier is answered exactly, not fuzzily.
    #[test]
    fn an_exact_symbol_is_never_answered_by_the_fuzzy_tier() {
        let mut functions: Vec<FunctionFact> = ["_Z5twiceIiET_S0_", "twice"]
            .into_iter()
            .map(|symbol| function("m", symbol, true, Linkage::External))
            .collect();
        functions.push(function("m", "other", true, Linkage::External));
        let session = Session::new(facts(functions, Vec::new()), Vec::new());

        let resolved = session.resolve("twice").expect("the C symbol `twice`");
        assert_eq!(resolved.matched, NameMatch::Mangled);
        assert_eq!(resolved.ids.len(), 1);
        assert_eq!(resolved.ids[0].symbol, "twice");
    }

    #[test]
    fn an_identifier_search_respects_both_edges() {
        assert!(contains_identifier("int twice<int>(int)", "twice"));
        assert!(contains_identifier("void wrap::twice()", "twice"));
        assert!(contains_identifier("twice_helper()", "twice_helper"));
        assert!(contains_identifier("void Foo::bar(int)", "Foo::bar"));

        assert!(
            !contains_identifier("twice_helper()", "twice"),
            "trailing _"
        );
        assert!(!contains_identifier("mytwice()", "twice"), "leading letter");
        assert!(!contains_identifier("int twice<int>(int)", ""));
    }

    #[test]
    fn uses_reports_the_heuristic_inventory_verbatim() {
        let session = session_with_address_taken_function();
        let uses = session.uses();
        let used: Vec<&str> = uses.iter().map(|u| u.used.symbol.as_str()).collect();
        assert_eq!(used, ["add", "log"]);
    }
}
