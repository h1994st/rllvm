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
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::catalog::{CatalogOrigin, CatalogScope};

pub mod extract;
pub use extract::{ModuleFacts, llvm_version};

pub mod facts;
pub use facts::*;

pub mod load;

pub mod bind;
pub use bind::{BindingCandidate, BindingStatus, SymbolBinding};

pub mod index;
pub use index::{Direction, PathStep, ReachResult, Session};

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
    Reach(Vec<PathStep>),
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
            QueryResults::Reach(items) => items.is_empty(),
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
    pub ambiguous_bindings: usize,
    /// For `reach`, the ambiguous bindings the walk actually reached. For
    /// every other query, every ambiguous binding in the selected scope.
    pub frontier: Vec<SymbolBinding>,
    pub truncated: bool,
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

/// Answer one query over an already-loaded session.
pub fn run(session: &Session, query: &Query) -> QueryResult {
    let mut reach_frontier: Option<Vec<SymbolBinding>> = None;

    let results = match query {
        Query::Defs { name } => QueryResults::Defs(defs(session, name)),
        Query::At { file, line } => QueryResults::At(at_entries(session, Path::new(file), *line)),
        Query::Callers { name } => QueryResults::Callers(callers(session, name)),
        Query::Callees { name } => QueryResults::Callees(session.callees(name)),
        Query::Uses { name } => QueryResults::Uses(uses_of(session, name)),
        Query::Reach { from, to } => {
            let reach = session.reach(from, to);
            reach_frontier = Some(reach.frontier);
            QueryResults::Reach(reach.path.unwrap_or_default())
        }
        Query::Closure { name, direction } => {
            QueryResults::Closure(session.closure(name, *direction))
        }
        Query::Externals => QueryResults::Externals(externals(session)),
        Query::IndirectTargets { at, heuristics } => {
            QueryResults::IndirectTargets(indirect_targets(session, at, *heuristics))
        }
    };

    // Reach reports exactly the ambiguous bindings its own walk hit, which
    // may legitimately be empty even while other bindings elsewhere in
    // scope are ambiguous. Every other query has no walk of its own, so it
    // reports the full program-wide set instead.
    let frontier = reach_frontier.unwrap_or_else(|| {
        session
            .bindings()
            .iter()
            .filter(|binding| binding.status == BindingStatus::Ambiguous)
            .cloned()
            .collect()
    });

    QueryResult {
        schema_version: 1,
        query: query.clone(),
        results,
        scope: session.scope().clone(),
        analysis: analysis_of(session.modules()),
        uncertainty: uncertainty_of(session, frontier),
        provenance: Provenance {
            catalog_origin: session.origin().clone(),
            llvm_version: llvm_version(),
            rllvm_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    }
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

fn indirect_targets(session: &Session, at: &str, heuristics: bool) -> Vec<IndirectTargetsResult> {
    let Some((file, line)) = parse_location(at) else {
        return Vec::new();
    };

    // Computed once whether or not any site below needs it, but never
    // exposed unless `heuristics` was requested: the whole point is that it
    // must not leak into a default answer.
    let inventory = heuristics.then(|| address_taken_inventory(session));
    let assumptions = vec![
        "Soundness holds only within the captured scope.".to_string(),
        "dlopen and a callback registered by code outside the captured scope both escape this bound.".to_string(),
    ];

    session
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
        .collect()
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
/// colon earlier does not shift the parse. A location that does not parse
/// answers no results, the same failure mode as one that matches nothing.
fn parse_location(at: &str) -> Option<(PathBuf, u32)> {
    let (file, line) = at.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((PathBuf::from(file), line))
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

fn uncertainty_of(session: &Session, frontier: Vec<SymbolBinding>) -> Uncertainty {
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
        ambiguous_bindings: frontier.len(),
        frontier,
        truncated: false,
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
        let result = run(&Session::new(facts, vec![]), &Query::Externals);
        assert_eq!(result.scope.selected_entries, 2);
        assert_eq!(result.analysis.analyzed, 1);
        assert_eq!(result.analysis.failed, 1);
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
        );
        assert!(result.results.is_empty());
        assert_eq!(result.uncertainty.indirect_call_sites, 1);
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
        );
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
        );
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
        let session = session_from_source_lines(&[("t.c", 2)]);
        let result = run(
            &session,
            &Query::At {
                file: "t.c".into(),
                line: 99,
            },
        );
        assert!(result.results.is_empty());
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
