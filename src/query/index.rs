//! Indexes over extracted facts, and the graph walks that answer
//! reachability.
//!
//! Every walk here distinguishes exactly three kinds of call-site evidence:
//! a `CallTarget::Direct` edge, a `CallTarget::Indirect` edge CVP bounded to
//! a known set of targets (traversed, one edge per bound member), and a
//! `CallTarget::Indirect` edge with no bound (uncertainty, never traversed).
//! The heuristic address-taken inventory recorded in `ProgramFacts::uses` is
//! not an edge source at all and is never consulted here.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::catalog::{CatalogOrigin, CatalogScope};

use super::{
    bind::{BindingStatus, SymbolBinding},
    facts::{
        CallSiteFact, CallSiteId, CallTarget, FunctionFact, FunctionId, ModuleReport, ProgramFacts,
        UseFact,
    },
};

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
    /// Bindings keyed by symbol, for resolving a declaration forward to its
    /// candidates.
    bindings_by_symbol: HashMap<String, Vec<usize>>,
    /// `Unique` bindings keyed by their one candidate, for reverse (`In`)
    /// closure walks back through the declaration that resolved to it.
    bindings_by_candidate: HashMap<FunctionId, Vec<usize>>,
    /// Functions mapped to a source file and line. Read by `functions_at`.
    by_file_line: HashMap<(PathBuf, u32), Vec<FunctionId>>,
}

impl Session {
    pub fn new(facts: ProgramFacts, bindings: Vec<SymbolBinding>) -> Session {
        let mut by_name: HashMap<String, Vec<FunctionId>> = HashMap::new();
        let mut function_index: HashMap<FunctionId, usize> = HashMap::new();
        let mut by_file_line: HashMap<(PathBuf, u32), Vec<FunctionId>> = HashMap::new();
        for (idx, function) in facts.functions.iter().enumerate() {
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
        for (idx, site) in facts.call_sites.iter().enumerate() {
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
                    ..
                }
                | CallTarget::Intrinsic { .. }
                | CallTarget::InlineAsm => {}
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

        Session {
            facts,
            bindings,
            by_name,
            function_index,
            callees_by_function,
            callers_by_function,
            bindings_by_symbol,
            bindings_by_candidate,
            by_file_line,
        }
    }

    /// Call sites that resolve to the named function: a direct call, or an
    /// indirect call CVP bounded to a set that includes it.
    pub fn callers(&self, name: &str) -> Vec<CallSiteFact> {
        self.ids_by_name(name)
            .iter()
            .flat_map(|id| self.callers_by_function.get(id).into_iter().flatten())
            .map(|&idx| self.facts.call_sites[idx].clone())
            .collect()
    }

    /// Call sites the named function makes, resolved or not.
    pub fn callees(&self, name: &str) -> Vec<CallSiteFact> {
        self.ids_by_name(name)
            .iter()
            .flat_map(|id| self.callees_by_function.get(id).into_iter().flatten())
            .map(|&idx| self.facts.call_sites[idx].clone())
            .collect()
    }

    /// Functions whose recorded source mapping includes `file:line`. Not
    /// public API: internal plumbing for the `at` query, called by
    /// `query::run`.
    pub(crate) fn functions_at(&self, file: &Path, line: u32) -> &[FunctionId] {
        self.by_file_line
            .get(&(file.to_path_buf(), line))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The heuristic address-taken inventory, verbatim. Never consulted by
    /// `reach`/`closure`; exposed only so an opt-in `indirect-targets`
    /// answer can report it. Not public API: internal plumbing, called by
    /// `query::run`.
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
    /// declared. Not public API: internal plumbing for `query::run`.
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

    /// Call sites recorded at exactly `file:line`, in any function. Not
    /// public API: internal plumbing for `at` and `indirect-targets`, which
    /// both key off a source location rather than a symbol name.
    pub(crate) fn call_sites_at(&self, file: &Path, line: u32) -> Vec<&CallSiteFact> {
        self.facts
            .call_sites
            .iter()
            .filter(|site| {
                site.location
                    .as_ref()
                    .is_some_and(|location| location.file == *file && location.line == line)
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
                return ReachResult {
                    path: Some(path),
                    frontier,
                };
            }

            let (edges, ambiguous) = self.successors(&current);
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

        ReachResult {
            path: None,
            frontier,
        }
    }

    /// The same walk as `reach`, run to exhaustion in one direction and
    /// collecting every function reached (never including the start set
    /// itself).
    pub fn closure(&self, name: &str, direction: Direction) -> Vec<FunctionId> {
        let mut visited: HashSet<FunctionId> = HashSet::new();
        let mut queue: VecDeque<FunctionId> = VecDeque::new();
        for start in self.ids_by_name(name) {
            if visited.insert(start.clone()) {
                queue.push_back(start);
            }
        }

        let mut reached = Vec::new();
        while let Some(current) = queue.pop_front() {
            let neighbors = match direction {
                Direction::Out => self.successors(&current).0,
                Direction::In => self.predecessors(&current),
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
        self.by_name.get(name).cloned().unwrap_or_default()
    }

    fn is_definition(&self, id: &FunctionId) -> bool {
        self.function_index
            .get(id)
            .map(|&idx| self.facts.functions[idx].is_definition)
            .unwrap_or(false)
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

    /// Forward edges out of `id`: a definition's resolved call sites, or a
    /// declaration's `Unique` binding. Also returns the binding at `id` when
    /// it is `Ambiguous`, so callers can record it in a `frontier` without
    /// treating it as an edge.
    fn successors(&self, id: &FunctionId) -> (Vec<(FunctionId, PathStep)>, Option<SymbolBinding>) {
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
        (edges, None)
    }

    /// Reverse edges into `id`: call sites resolving to it, and any `Unique`
    /// binding for which it is the one candidate, walked back to the
    /// declaration(s) that resolve to it.
    fn predecessors(&self, id: &FunctionId) -> Vec<(FunctionId, PathStep)> {
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
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{facts::Linkage, testing::*};

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

    #[test]
    fn functions_at_finds_only_the_mapped_line() {
        let session = session_from_source_lines(&[("t.c", 2)]);

        let found = session.functions_at(Path::new("t.c"), 2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].symbol, "only");
        assert!(session.functions_at(Path::new("t.c"), 99).is_empty());
    }

    #[test]
    fn uses_reports_the_heuristic_inventory_verbatim() {
        let session = session_with_address_taken_function();
        let uses = session.uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].used.symbol, "add");
    }
}
