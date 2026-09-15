//! Source-level queries over captured bitcode.
//!
//! Every answer is wrapped in [`QueryResult`], this project's honesty
//! surface. Four rules hold for every query, not just the ones that
//! obviously need them:
//!
//! 1. `scope` is quoted from the catalog and never shrinks. A module that
//!    failed to parse, went missing, or failed hash verification is counted
//!    in the separate `analysis` block instead. Merging the two would let a
//!    parse failure silently narrow the program the answer claims to
//!    describe.
//! 2. `analysis.modules` carries `ir_stage` and `debug_info` per module, not
//!    as one aggregate: a mixed catalog is ordinary, and one summary flag
//!    would misrepresent it.
//! 3. Indirect call sites are never dropped from a `callees` answer. They
//!    appear as unresolved with their locations, so the answer does not read
//!    as a complete list of what a function calls.
//! 4. An empty `reach` result is not unreachability. It means "no path over
//!    resolved edges within the selected scope", and `uncertainty` names the
//!    indirect sites and ambiguous bindings that could carry a path the walk
//!    cannot see.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    catalog::{CatalogOrigin, CatalogScope},
    error::Error,
};

pub mod extract;
pub use extract::{ModuleFacts, llvm_version};

pub mod facts;
pub use facts::*;

pub mod load;

pub mod bind;
pub use bind::{BindingCandidate, BindingStatus, SymbolBinding};

pub mod index;
pub use index::{Direction, PathStep, ReachResult, Session};

pub mod mcp;

#[cfg(test)]
pub(crate) mod testing;

/// One of the nine source-level queries. Serializes with a `kind` tag, e.g.
/// `{"kind": "callers", "name": "parse_frame"}`.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    /// Every definition of the symbol, with module and configuration.
    Defs { name: String },
    /// Functions with at least one instruction mapped to `file:line`, and
    /// the call sites recorded there. Deliberately not source-range
    /// containment: a line that generated no instructions answers empty
    /// rather than guessing at a containing function.
    At { file: String, line: u32 },
    /// Functions containing a call to the target, each with its call sites.
    Callers { name: String },
    /// Outgoing call sites of the target, classified. Includes unresolved
    /// indirect sites: they are evidence of uncertainty, not omitted.
    Callees { name: String },
    /// Non-call uses: how and where the function's address is taken.
    Uses { name: String },
    /// One supporting path from `from` to `to`, or its explicit absence.
    /// Enumerating every path is out of scope.
    Reach { from: String, to: String },
    /// The set that can reach the target (`In`) or that it can reach
    /// (`Out`).
    Closure { name: String, direction: Direction },
    /// Unbound symbols: the captured program's boundary.
    Externals,
    /// `!callees` at a call site, when CVP produced it; otherwise
    /// unresolved. `at` is a `file:line` location, e.g. `"t.c:4"`.
    IndirectTargets { at: String, heuristics: bool },
}

/// One definition of a queried symbol.
#[derive(Debug, Serialize)]
pub struct DefEntry {
    pub function: FunctionId,
    /// Quoted from the defining module's `ModuleReport`, not reconstructed.
    pub configuration_id: Option<String>,
    pub location: Option<SourceLocation>,
}

/// One function found at a queried location, with the call sites it makes
/// there.
#[derive(Debug, Serialize)]
pub struct AtEntry {
    pub function: FunctionId,
    pub call_sites: Vec<CallSiteFact>,
}

/// One function calling the queried target, with its call sites.
#[derive(Debug, Serialize)]
pub struct CallerEntry {
    pub function: FunctionId,
    pub call_sites: Vec<CallSiteFact>,
}

/// The answer to `indirect-targets`, built from three fields that are never
/// merged.
#[derive(Debug, Serialize)]
pub struct IndirectTargetsResult {
    pub site: CallSiteId,
    pub location: Option<SourceLocation>,
    pub signature: String,
    /// From `!callees`. An LLVM-derived upper bound: a defined execution of
    /// the call cannot target a function outside this set. Absent when CVP
    /// could not bound the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llvm_target_bound: Option<Vec<FunctionId>>,
    /// True exactly when `llvm_target_bound` is absent.
    pub unresolved: bool,
    /// Opt-in only: `None` unless `heuristics` was requested. Derived from
    /// `ProgramFacts::uses`: a function with any recorded use has had its
    /// address taken somewhere in scope. A heuristic inventory, not a
    /// result, and never a source of graph edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_taken_inventory: Option<Vec<FunctionId>>,
    /// Present only alongside the unfiltered `address_taken_inventory`,
    /// never instead of it: casting function pointers is routine in C.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_compatible: Option<Vec<FunctionId>>,
    /// States the bound this scope's soundness: `dlopen` and a callback
    /// registered by uncaptured code both escape it.
    pub assumptions: Vec<String>,
}

/// The per-query result list. Untagged: each query's own shape serializes
/// directly as the `results` array, with no wrapper variant name.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QueryResults {
    Defs(Vec<DefEntry>),
    At(Vec<AtEntry>),
    Callers(Vec<CallerEntry>),
    Callees(Vec<CallSiteFact>),
    Uses(Vec<UseFact>),
    /// `None` when no path over resolved edges exists. `Some` carries the
    /// path's steps, which may legitimately be empty: `reach(x, x)` finds
    /// `x` immediately and returns `Some(vec![])`, a found answer, not an
    /// absent one. Collapsing the two into one `Vec` would make a trivial
    /// found path read as unreachable.
    Reach(Option<Vec<PathStep>>),
    Closure(Vec<FunctionId>),
    Externals(Vec<SymbolBinding>),
    IndirectTargets(Vec<IndirectTargetsResult>),
}

impl QueryResults {
    pub fn is_empty(&self) -> bool {
        match self {
            QueryResults::Defs(items) => items.is_empty(),
            QueryResults::At(items) => items.is_empty(),
            QueryResults::Callers(items) => items.is_empty(),
            QueryResults::Callees(items) => items.is_empty(),
            QueryResults::Uses(items) => items.is_empty(),
            // `None` (no path) is the only "nothing to report" case;
            // `Some(_)` is a found answer even when its step list is
            // itself empty (a trivial `reach(x, x)`).
            QueryResults::Reach(path) => path.is_none(),
            QueryResults::Closure(items) => items.is_empty(),
            QueryResults::Externals(items) => items.is_empty(),
            QueryResults::IndirectTargets(items) => items.is_empty(),
        }
    }
}

/// Per-status module counts, plus the per-module detail they summarize.
/// Counted here, never in `scope`: a parse failure or a missing module must
/// not narrow the program `scope` claims to describe.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Analysis {
    /// Read and hash-verified, but not extracted. [`open`] promotes every
    /// verified module to `analyzed` or `failed` before it returns, so this
    /// is 0 for a session opened from a catalog; it is non-zero only for a
    /// [`Session`] a caller assembled itself from [`ModuleReport`]s that
    /// extraction never saw. Counted rather than dropped so `analysis` stays
    /// total over [`ModuleAnalysis`]: a status with no count would let a
    /// module disappear from the summary entirely.
    pub verified: usize,
    pub analyzed: usize,
    pub changed: usize,
    pub missing: usize,
    pub failed: usize,
    pub unsupported: usize,
    pub not_built: usize,
    /// `ir_stage` and `debug_info` per module. A single aggregate would
    /// misrepresent a mixed catalog, which the compilation-database import
    /// makes an ordinary case.
    pub modules: Vec<ModuleReport>,
}

/// What the answer could not see. Present on every answer, not only walks:
/// a `callees` or `defs` answer is read alongside the same uncertainty a
/// `reach` answer would report for the same program.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Uncertainty {
    pub indirect_call_sites: usize,
    pub sites_with_llvm_target_bound: usize,
    pub functions_without_location: usize,
    pub locations_from_modified_sources: usize,
    /// Every ambiguous binding in the selected scope, on every query
    /// including `reach`. One meaning, program-wide: it is not the length of
    /// `frontier`, which for `reach` reports the narrower set that one walk
    /// actually reached.
    pub ambiguous_bindings: usize,
    /// For `reach`, the ambiguous bindings the walk actually reached, one
    /// entry per symbol. For every other query, every ambiguous binding in
    /// the selected scope.
    pub frontier: Vec<SymbolBinding>,
    /// Steps of a returned `reach` path that are `bounded_indirect`, and so
    /// hold only if the call takes the member the path chose. Zero for a
    /// path of direct calls and resolved bindings, and for every query that
    /// returns no path: a non-zero count says *this* answer is conditional
    /// without the reader having to walk the step kinds.
    pub conditional_path_steps: usize,
}

/// Where the answer's facts came from.
#[derive(Clone, Debug, Serialize)]
pub struct Provenance {
    /// Quoted from the catalog, not reconstructed.
    pub catalog_origin: CatalogOrigin,
    pub llvm_version: String,
    pub rllvm_version: String,
}

/// The envelope every query answer is wrapped in.
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub schema_version: u32,
    pub query: Query,
    pub results: QueryResults,
    /// Cloned from `ProgramFacts::scope`, never recomputed.
    pub scope: CatalogScope,
    pub analysis: Analysis,
    pub uncertainty: Uncertainty,
    pub provenance: Provenance,
}

/// Load a catalog into a [`Session`] ready to answer queries: read and
/// verify every module it names, extract facts from each, and resolve
/// cross-module symbol bindings.
///
/// Assembly order matters, and enforces two rules. First, only extraction
/// may promote a module's report from `Verified` (set by
/// [`load::load_catalog`]) to `Analyzed`; a module that fails to extract, or
/// that cannot be read at all by the time its bytes are wanted, is marked
/// `Failed` with the diagnostic instead, and the run continues -- the other
/// modules still answer. Second, [`load::for_each_module`] hands over one
/// module's bytes at a time by design, and the `Loaded` value is dropped
/// before the session is built, so no bitcode buffer stays resident once
/// queries start answering.
pub fn open(catalog: &Path) -> Result<Session, Error> {
    let loaded = load::load_catalog(catalog)?;

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

    let unreadable = load::for_each_module(&loaded, |module| {
        match extract::extract(&module, &loaded.source_status) {
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
                record_failure(&mut reports, &module.id, error.to_string());
            }
        }
        // A module that fails to extract must not abort the run.
        Ok(())
    })?;

    // Nor may a module that verified at load time and then vanished or
    // became unreadable: it is recorded in `analysis` like any other
    // failure, and every other module still answers.
    for (id, error) in unreadable {
        tracing::warn!(module = %id, %error, "module could not be read");
        record_failure(&mut reports, &id, error.to_string());
    }

    let bindings = bind::bind(&functions, &configurations);
    let facts = ProgramFacts {
        functions,
        call_sites,
        uses,
        scope: loaded.scope.clone(),
        origin: loaded.origin.clone(),
        modules: reports,
    };
    // `for_each_module` already dropped its own archive cache on return;
    // this drops `Loaded` itself before the session below starts serving.
    drop(loaded);

    Ok(Session::new(facts, bindings))
}

/// Answer one query over an already-loaded session.
///
/// Fails only on a query that cannot be interpreted -- an `indirect-targets`
/// location that does not parse as `file:line`. A query that is understood
/// but finds nothing is an answer, not an error, and comes back as an empty
/// `results` list.
pub fn run(session: &Session, query: &Query) -> Result<QueryResult, Error> {
    let mut reach_frontier: Option<Vec<SymbolBinding>> = None;
    let mut conditional_path_steps = 0;

    let results = match query {
        Query::Defs { name } => QueryResults::Defs(defs(session, name)),
        Query::At { file, line } => QueryResults::At(at_entries(session, Path::new(file), *line)),
        Query::Callers { name } => QueryResults::Callers(callers(session, name)),
        Query::Callees { name } => QueryResults::Callees(session.callees(name)),
        Query::Uses { name } => QueryResults::Uses(uses_of(session, name)),
        Query::Reach { from, to } => {
            let reach = session.reach(from, to);
            reach_frontier = Some(reach.frontier);
            conditional_path_steps = reach
                .path
                .iter()
                .flatten()
                .filter(|step| matches!(step, PathStep::BoundedIndirect { .. }))
                .count();
            QueryResults::Reach(reach.path)
        }
        Query::Closure { name, direction } => {
            QueryResults::Closure(session.closure(name, *direction))
        }
        Query::Externals => QueryResults::Externals(externals(session)),
        Query::IndirectTargets { at, heuristics } => {
            QueryResults::IndirectTargets(indirect_targets(session, at, *heuristics)?)
        }
    };

    // Reach reports exactly the ambiguous bindings its own walk hit, which
    // may legitimately be empty even while other bindings elsewhere in
    // scope are ambiguous. Every other query has no walk of its own, so it
    // reports the full program-wide set instead. The `ambiguous_bindings`
    // count is program-wide either way -- see `uncertainty_of`.
    let frontier = reach_frontier.unwrap_or_else(|| ambiguous_bindings(session));

    Ok(QueryResult {
        schema_version: 1,
        query: query.clone(),
        results,
        scope: session.scope().clone(),
        analysis: analysis_of(session.modules()),
        uncertainty: uncertainty_of(session, frontier, conditional_path_steps),
        provenance: Provenance {
            catalog_origin: session.origin().clone(),
            llvm_version: llvm_version(),
            rllvm_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    })
}

/// Marks one module's report `Failed` with the reason. Only extraction may
/// write `Analyzed`; every other outcome for a module the loader verified
/// lands here.
fn record_failure(reports: &mut [ModuleReport], id: &str, reason: String) {
    if let Some(report) = reports.iter_mut().find(|report| report.id == id) {
        report.status = ModuleAnalysis::Failed;
        report.diagnostic = Some(reason);
    }
}

/// Every ambiguous binding in the selected scope, one entry per symbol.
fn ambiguous_bindings(session: &Session) -> Vec<SymbolBinding> {
    session
        .bindings()
        .iter()
        .filter(|binding| binding.status == BindingStatus::Ambiguous)
        .cloned()
        .collect()
}

fn defs(session: &Session, name: &str) -> Vec<DefEntry> {
    session
        .definitions(name)
        .into_iter()
        .map(|function| DefEntry {
            function: function.id.clone(),
            configuration_id: configuration_of(session, &function.id.module_id),
            location: function.location.clone(),
        })
        .collect()
}

fn configuration_of(session: &Session, module_id: &str) -> Option<String> {
    session
        .modules()
        .iter()
        .find(|module| module.id == module_id)
        .and_then(|module| module.configuration_id.clone())
}

fn at_entries(session: &Session, file: &Path, line: u32) -> Vec<AtEntry> {
    let call_sites = session.call_sites_at(file, line);
    session
        .functions_at(file, line)
        .iter()
        .map(|id| {
            let call_sites = call_sites
                .iter()
                .filter(|site| &site.id.function == id)
                .map(|site| (*site).clone())
                .collect();
            AtEntry {
                function: id.clone(),
                call_sites,
            }
        })
        .collect()
}

fn callers(session: &Session, name: &str) -> Vec<CallerEntry> {
    let mut grouped: BTreeMap<FunctionId, Vec<CallSiteFact>> = BTreeMap::new();
    for site in session.callers(name) {
        grouped
            .entry(site.id.function.clone())
            .or_default()
            .push(site);
    }
    grouped
        .into_iter()
        .map(|(function, call_sites)| CallerEntry {
            function,
            call_sites,
        })
        .collect()
}

fn uses_of(session: &Session, name: &str) -> Vec<UseFact> {
    session
        .uses()
        .iter()
        .filter(|use_fact| use_fact.used.symbol == name)
        .cloned()
        .collect()
}

fn externals(session: &Session) -> Vec<SymbolBinding> {
    session
        .bindings()
        .iter()
        .filter(|binding| binding.status == BindingStatus::Unbound)
        .cloned()
        .collect()
}

fn indirect_targets(
    session: &Session,
    at: &str,
    heuristics: bool,
) -> Result<Vec<IndirectTargetsResult>, Error> {
    let (file, line) = parse_location(at)?;

    // Computed once whether or not any site below needs it, but never
    // exposed unless `heuristics` was requested: the whole point is that it
    // must not leak into a default answer.
    let inventory = heuristics.then(|| address_taken_inventory(session));
    let assumptions = vec![
        "Soundness holds only within the captured scope.".to_string(),
        "dlopen and a callback registered by code outside the captured scope both escape this bound.".to_string(),
    ];

    Ok(session
        .call_sites_at(&file, line)
        .into_iter()
        .filter_map(|site| {
            let CallTarget::Indirect {
                signature,
                llvm_target_bound,
            } = &site.target
            else {
                return None;
            };
            let signature_compatible = inventory.as_ref().map(|functions| {
                functions
                    .iter()
                    .filter(|id| {
                        session
                            .function(id)
                            .is_some_and(|function| &function.signature == signature)
                    })
                    .cloned()
                    .collect()
            });
            Some(IndirectTargetsResult {
                site: site.id.clone(),
                location: site.location.clone(),
                signature: signature.clone(),
                unresolved: llvm_target_bound.is_none(),
                llvm_target_bound: llvm_target_bound.clone(),
                address_taken_inventory: inventory.clone(),
                signature_compatible,
                assumptions: assumptions.clone(),
            })
        })
        .collect())
}

/// Every function with at least one recorded use, deduplicated. Not filtered
/// by signature: that filtering is `signature_compatible`'s job, reported
/// only alongside this unfiltered set.
fn address_taken_inventory(session: &Session) -> Vec<FunctionId> {
    let mut seen: BTreeSet<FunctionId> = BTreeSet::new();
    let mut inventory = Vec::new();
    for use_fact in session.uses() {
        if seen.insert(use_fact.used.clone()) {
            inventory.push(use_fact.used.clone());
        }
    }
    inventory
}

/// Splits a `file:line` location on its last colon, so a path containing a
/// colon earlier does not shift the parse.
///
/// A location that does not parse is an error, not an empty answer.
/// `indirect-targets` exists to surface what cannot be resolved, so a typo
/// that answered `results: []` would be byte-identical to a valid line with
/// no indirect calls -- and over MCP an agent client would have no way to
/// tell the two apart.
fn parse_location(at: &str) -> Result<(PathBuf, u32), Error> {
    let invalid = || {
        Error::InvalidArguments(format!(
            "invalid location `{at}`: expected `file:line`, e.g. `parser.c:8`"
        ))
    };
    let (file, line) = at.rsplit_once(':').ok_or_else(invalid)?;
    let line: u32 = line.parse().map_err(|_| invalid())?;
    Ok((PathBuf::from(file), line))
}

fn analysis_of(modules: &[ModuleReport]) -> Analysis {
    let mut analysis = Analysis {
        modules: modules.to_vec(),
        ..Default::default()
    };
    for module in modules {
        match module.status {
            ModuleAnalysis::Verified => analysis.verified += 1,
            ModuleAnalysis::Analyzed => analysis.analyzed += 1,
            ModuleAnalysis::Changed => analysis.changed += 1,
            ModuleAnalysis::Missing => analysis.missing += 1,
            ModuleAnalysis::Failed => analysis.failed += 1,
            ModuleAnalysis::Unsupported => analysis.unsupported += 1,
            ModuleAnalysis::NotBuilt => analysis.not_built += 1,
        }
    }
    analysis
}

fn uncertainty_of(
    session: &Session,
    frontier: Vec<SymbolBinding>,
    conditional_path_steps: usize,
) -> Uncertainty {
    let call_sites = session.call_sites();
    let indirect_call_sites = call_sites
        .iter()
        .filter(|site| matches!(&site.target, CallTarget::Indirect { .. }))
        .count();
    let sites_with_llvm_target_bound = call_sites
        .iter()
        .filter(|site| {
            matches!(
                &site.target,
                CallTarget::Indirect {
                    llvm_target_bound: Some(_),
                    ..
                }
            )
        })
        .count();
    let functions_without_location = session
        .functions()
        .iter()
        .filter(|function| function.location.is_none())
        .count();

    let is_modified = |location: &Option<SourceLocation>| {
        location
            .as_ref()
            .is_some_and(|location| location.source_status == SourceStatus::Modified)
    };
    let locations_from_modified_sources = session
        .functions()
        .iter()
        .filter(|function| is_modified(&function.location))
        .count()
        + call_sites
            .iter()
            .filter(|site| is_modified(&site.location))
            .count()
        + session
            .uses()
            .iter()
            .filter(|use_fact| is_modified(&use_fact.location))
            .count();

    Uncertainty {
        indirect_call_sites,
        sites_with_llvm_target_bound,
        functions_without_location,
        locations_from_modified_sources,
        // Program-wide on every query, `reach` included: `frontier.len()`
        // would mean "reached by this walk" here and "program-wide"
        // everywhere else, and a `reach` frontier counts one symbol once
        // however many declarations reached it.
        ambiguous_bindings: ambiguous_bindings(session).len(),
        frontier,
        conditional_path_steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{ModuleCatalog, ModuleRecord, ModuleStatus, hash_bytes, write_catalog},
        query::{load::load_catalog, testing::*},
    };

    #[test]
    fn a_module_is_analyzed_only_after_it_parses() {
        // The loader marks Verified; only extraction may promote to Analyzed.
        let scratch = tempfile::tempdir().unwrap();
        let (catalog_path, module_path) = write_catalog_with_one_module(&scratch);
        let loaded = load_catalog(&catalog_path).unwrap();
        assert_eq!(loaded.reports[0].status, ModuleAnalysis::Verified);
        let _ = module_path;
    }

    #[test]
    fn scope_counts_survive_a_failed_module() {
        let facts = facts_with_one_failed_module();
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        assert_eq!(result.scope.selected_entries, 2);
        assert_eq!(result.analysis.analyzed, 1);
        assert_eq!(result.analysis.failed, 1);
        // Per-module detail, not just a count: rule 2 requires `ir_stage`
        // and `debug_info` to survive per module rather than collapsing
        // into one aggregate flag.
        assert_eq!(result.analysis.modules[0].debug_info, Some(true));
    }

    #[test]
    fn an_empty_reach_names_the_indirect_sites_it_could_not_follow() {
        let session = session_with_indirect_gap();
        let result = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "c".into(),
            },
        )
        .unwrap();
        assert!(result.results.is_empty());
        assert_eq!(result.uncertainty.indirect_call_sites, 1);
    }

    #[test]
    fn a_trivial_reach_is_a_found_path_not_an_absent_one() {
        // `session.reach("a", "a")` finds `a` immediately and returns
        // `Some(vec![])`: a found path with zero steps. Collapsing that
        // into the same empty `Vec` a missing path produces would make a
        // trivial found path read as unreachable.
        let session = session_from(&[("a", "b")]);

        let trivial = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "a".into(),
            },
        )
        .unwrap();
        assert!(
            !trivial.results.is_empty(),
            "a==a must be a found path, not an absent one"
        );

        let missing = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "absent".into(),
            },
        )
        .unwrap();
        assert!(missing.results.is_empty());
    }

    #[test]
    fn heuristics_are_absent_unless_requested() {
        let session = session_with_address_taken_function();
        let result = run(
            &session,
            &Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics: false,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.to_string().find("address_taken_inventory").is_none());
    }

    #[test]
    fn requested_heuristics_stay_in_their_own_field() {
        let session = session_with_address_taken_function();
        let result = run(
            &session,
            &Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics: true,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&result).unwrap();
        let entry = &json["results"][0];
        assert!(
            entry["address_taken_inventory"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["symbol"] == "add"),
            "inventory must list address-taken functions when requested"
        );
        assert!(
            entry["llvm_target_bound"].is_null(),
            "a heuristic must never appear as an LLVM-provided bound"
        );
    }

    #[test]
    fn at_returns_nothing_for_a_line_with_no_instructions() {
        // Lines 2 and 10 are mapped, but 5 is not: a range-containment
        // implementation (`min..max`) would wrongly return the function for
        // line 5, since it falls inside `2..10`. Only a mapping
        // implementation answers empty here, which is `at`'s actual
        // contract: at least one instruction mapped to that exact line.
        let session = session_from_source_lines(&[("t.c", 2), ("t.c", 10)]);
        let result = run(
            &session,
            &Query::At {
                file: "t.c".into(),
                line: 5,
            },
        )
        .unwrap();
        assert!(result.results.is_empty());
    }

    #[test]
    fn callees_of_b_keeps_the_unresolved_indirect_site() {
        // A future refactor could add a `CallTarget::Indirect { .. } => {}`
        // arm to `callees_by_function` by symmetry with the match just
        // below it for `callers_by_function` (`index.rs`), which would
        // drop every unresolved indirect site from every `callees` answer
        // without failing any existing test. This pins that it must not.
        let session = session_with_indirect_gap();
        let result = run(&session, &Query::Callees { name: "b".into() }).unwrap();
        let QueryResults::Callees(sites) = &result.results else {
            panic!("Query::Callees must produce QueryResults::Callees");
        };
        let indirect = sites
            .iter()
            .find(|site| matches!(&site.target, CallTarget::Indirect { .. }))
            .expect("the unresolved indirect call site must not be dropped");
        assert_eq!(indirect.id.instruction_index, 1);
        assert_eq!(indirect.location, None);
    }

    #[test]
    fn ambiguous_bindings_counts_the_scope_not_the_walk() {
        // Two ambiguous bindings in scope; the walk from `caller` reaches
        // one. `ambiguous_bindings` has to mean the same thing on both
        // answers, or a reader comparing two queries over one catalog sees
        // two different numbers for one program.
        let session = session_with_ambiguous_bindings();

        let reach = run(
            &session,
            &Query::Reach {
                from: "caller".into(),
                to: "target".into(),
            },
        )
        .unwrap();
        assert_eq!(
            reach.uncertainty.frontier.len(),
            1,
            "the frontier is what this walk reached"
        );
        assert_eq!(
            reach.uncertainty.ambiguous_bindings, 2,
            "the count is program-wide, not the frontier's length"
        );

        let externals = run(&session, &Query::Externals).unwrap();
        assert_eq!(externals.uncertainty.ambiguous_bindings, 2);
    }

    #[test]
    fn a_path_through_a_bounded_indirect_call_is_flagged_conditional() {
        // The path holds only if the call takes the member it chose. Without
        // a count in `uncertainty`, a reader has to walk the step kinds to
        // learn that, and an agent client reading the envelope will not.
        let conditional = run(
            &session_with_bounded_indirect(),
            &Query::Reach {
                from: "a".into(),
                to: "target".into(),
            },
        )
        .unwrap();
        assert!(!conditional.results.is_empty(), "a path must be found");
        assert_eq!(conditional.uncertainty.conditional_path_steps, 1);

        let direct = run(
            &session_from(&[("a", "b"), ("b", "c")]),
            &Query::Reach {
                from: "a".into(),
                to: "c".into(),
            },
        )
        .unwrap();
        assert!(!direct.results.is_empty());
        assert_eq!(
            direct.uncertainty.conditional_path_steps, 0,
            "a path of direct calls is not conditional"
        );
    }

    #[test]
    fn an_unparseable_location_is_an_error_not_an_empty_answer() {
        // `indirect-targets parser.c` (no `:8`) must not answer `results:
        // []`, which is byte-identical to a valid line with no indirect
        // calls -- for the one query whose purpose is surfacing what cannot
        // be resolved.
        let session = session_with_address_taken_function();
        let error = run(
            &session,
            &Query::IndirectTargets {
                at: "parser.c".into(),
                heuristics: false,
            },
        )
        .expect_err("a location without a line must not answer");
        assert!(error.to_string().contains("parser.c"), "{error}");

        assert!(
            run(
                &session,
                &Query::IndirectTargets {
                    at: "parser.c:notaline".into(),
                    heuristics: false,
                },
            )
            .is_err(),
            "a non-numeric line must not answer either"
        );
    }

    #[test]
    fn a_module_extraction_never_saw_is_counted_verified() {
        // `open` promotes every verified module, so this count is 0 there;
        // it is reachable for a `Session` a caller assembles itself, and
        // dropping the field would leave `ModuleAnalysis::Verified`
        // uncounted in `analysis` rather than reported as zero.
        let result = run(
            &Session::new(facts_with_one_verified_module(), vec![]),
            &Query::Externals,
        )
        .unwrap();
        assert_eq!(result.analysis.verified, 1);
        assert_eq!(result.analysis.analyzed, 0);
        assert_eq!(
            result.scope.selected_entries, 1,
            "scope still quotes the catalog"
        );
    }

    /// A synthetic one-module catalog whose module bytes are not real
    /// bitcode: `load_catalog` only verifies bytes against the recorded
    /// hash and never parses, so arbitrary content with a matching hash is
    /// enough to exercise the Verified status this test pins.
    fn write_catalog_with_one_module(scratch: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let module_path = scratch.path().join("m.bc");
        let bytes = b"not real bitcode, only its hash matters here";
        std::fs::write(&module_path, bytes).unwrap();

        let mut record = ModuleRecord::new("m");
        record.path = Some(PathBuf::from("m.bc"));
        record.content_sha256 = Some(hash_bytes(bytes));
        record.status = ModuleStatus::Available;

        let catalog = ModuleCatalog::new(
            CatalogOrigin {
                kind: "test".into(),
                input: PathBuf::from("test"),
                sha256: None,
            },
            "test",
            vec![record],
        );
        let catalog_path = scratch.path().join("catalog.json");
        write_catalog(&catalog_path, &catalog).unwrap();
        (catalog_path, module_path)
    }
}
