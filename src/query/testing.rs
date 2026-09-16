//! Shared `ProgramFacts`/`Session` builders for `query` submodule tests.
//!
//! Kept in one module, rather than duplicated per test module, because Rust
//! test modules cannot import each other's `#[cfg(test)] mod tests` and more
//! than one submodule's tests build on these fixtures.

use std::path::PathBuf;

use crate::{
    catalog::{CatalogOrigin, CatalogScope},
    query::{
        bind::{BindingCandidate, BindingStatus, SymbolBinding},
        facts::{
            CallSiteFact, CallSiteId, CallTarget, FunctionFact, FunctionId, Linkage,
            ModuleAnalysis, ModuleReport, ProgramFacts, SourceLocation, SourceStatus, UseFact,
            UseKind,
        },
        index::Session,
    },
};

pub(crate) fn function(
    module: &str,
    symbol: &str,
    is_definition: bool,
    linkage: Linkage,
) -> FunctionFact {
    FunctionFact {
        id: FunctionId {
            module_id: module.into(),
            symbol: symbol.into(),
        },
        is_definition,
        linkage,
        signature: "i32 (i32, i32)".into(),
        location: None,
        mapped_lines: Default::default(),
    }
}

pub(crate) fn direct_call(
    caller: &FunctionFact,
    callee: &FunctionFact,
    index: u32,
) -> CallSiteFact {
    CallSiteFact {
        id: CallSiteId {
            function: caller.id.clone(),
            block_index: 0,
            instruction_index: index,
        },
        location: None,
        target: CallTarget::Direct {
            callee: callee.id.clone(),
        },
    }
}

/// An indirect call site in `caller`: `llvm_target_bound` is what CVP
/// managed to bound it to, or `None` for a site it could not bound.
pub(crate) fn indirect_call(
    caller: &FunctionFact,
    index: u32,
    location: Option<SourceLocation>,
    llvm_target_bound: Option<Vec<FunctionId>>,
) -> CallSiteFact {
    CallSiteFact {
        id: CallSiteId {
            function: caller.id.clone(),
            block_index: 0,
            instruction_index: index,
        },
        location,
        target: CallTarget::Indirect {
            signature: "i32 (i32, i32)".into(),
            llvm_target_bound,
        },
    }
}

/// A module report with only its id and status set; fixtures override the
/// one or two other fields they actually pin.
pub(crate) fn report(id: &str, status: ModuleAnalysis) -> ModuleReport {
    ModuleReport {
        id: id.into(),
        status,
        ir_stage: None,
        debug_info: None,
        compiler: None,
        configuration_id: None,
        content_sha256: None,
        target_triple: None,
        diagnostic: None,
    }
}

/// An ambiguous binding for `symbol`, declared in `declared_in` and defined
/// once per `(module, configuration)` candidate.
pub(crate) fn ambiguous_binding(
    symbol: &str,
    declared_in: &[&str],
    candidates: &[(&str, Option<&str>)],
) -> SymbolBinding {
    SymbolBinding {
        symbol: symbol.into(),
        declared_in: declared_in.iter().map(|id| (*id).to_string()).collect(),
        candidates: candidates
            .iter()
            .map(|(module, configuration)| BindingCandidate {
                function: FunctionId {
                    module_id: (*module).into(),
                    symbol: symbol.into(),
                },
                configuration_id: configuration.map(str::to_string),
            })
            .collect(),
        status: BindingStatus::Ambiguous,
    }
}

pub(crate) fn facts(functions: Vec<FunctionFact>, call_sites: Vec<CallSiteFact>) -> ProgramFacts {
    ProgramFacts {
        functions,
        call_sites,
        uses: Vec::new(),
        scope: CatalogScope {
            kind: "test".into(),
            total_entries: 1,
            selected_entries: 1,
            selection: Default::default(),
            selection_history: Vec::new(),
            analysis_arguments: Vec::new(),
            whole_program_complete: None,
            limitations: Vec::new(),
        },
        origin: CatalogOrigin {
            kind: "test".into(),
            input: PathBuf::from("test"),
            sha256: None,
        },
        modules: Vec::new(),
    }
}

/// One module, one edge per pair.
pub(crate) fn session_from(edges: &[(&str, &str)]) -> Session {
    let mut functions: Vec<FunctionFact> = Vec::new();
    let lookup = |functions: &mut Vec<FunctionFact>, name: &str| -> FunctionFact {
        if let Some(existing) = functions.iter().find(|f| f.id.symbol == name) {
            return existing.clone();
        }
        let created = function("m", name, true, Linkage::Internal);
        functions.push(created.clone());
        created
    };
    let mut call_sites = Vec::new();
    for (index, (from, to)) in edges.iter().enumerate() {
        let caller = lookup(&mut functions, from);
        let callee = lookup(&mut functions, to);
        call_sites.push(direct_call(&caller, &callee, index as u32));
    }
    Session::new(facts(functions, call_sites), Vec::new())
}

/// `a` reaches `target` only through an indirect site CVP bounded to
/// `{target, other}`. A second bound member matters: it lets a test tell a
/// correct bounded-indirect edge apart from one that collapsed the bound to
/// just the target taken.
pub(crate) fn session_with_bounded_indirect() -> Session {
    let a = function("m", "a", true, Linkage::Internal);
    let target = function("m", "target", true, Linkage::Internal);
    let other = function("m", "other", true, Linkage::Internal);
    let site = indirect_call(&a, 0, None, Some(vec![target.id.clone(), other.id.clone()]));
    Session::new(facts(vec![a, target, other], vec![site]), Vec::new())
}

/// `a` calls `b`; `b` reaches `c` only through an indirect call CVP could not
/// bound, so no path exists over direct edges.
pub(crate) fn session_with_indirect_gap() -> Session {
    let a = function("m", "a", true, Linkage::Internal);
    let b = function("m", "b", true, Linkage::Internal);
    let c = function("m", "c", true, Linkage::Internal);
    let call = direct_call(&a, &b, 0);
    let indirect = indirect_call(&b, 1, None, None);
    Session::new(facts(vec![a, b, c], vec![call, indirect]), Vec::new())
}

/// One indirect site at `t.c:4`, and an `add` whose address is taken.
pub(crate) fn session_with_address_taken_function() -> Session {
    let add = function("m", "add", true, Linkage::Internal);
    let caller = function("m", "caller", true, Linkage::Internal);
    let mut base = facts(
        vec![add.clone(), caller.clone()],
        vec![indirect_call(
            &caller,
            0,
            Some(SourceLocation {
                file: "t.c".into(),
                directory: None,
                line: 4,
                column: 3,
                source_status: SourceStatus::Unknown,
                inlined_at: Vec::new(),
            }),
            None,
        )],
    );
    base.uses = vec![UseFact {
        used: add.id,
        in_function: Some(caller.id),
        location: None,
        kind: UseKind::StoredToMemory,
    }];
    Session::new(base, Vec::new())
}

/// `scope.selected_entries == 2`, but only one module was read: `a` parsed,
/// `b` failed. Pins that `scope` is quoted from the catalog and never
/// recomputed from what `analysis` actually managed to read.
pub(crate) fn facts_with_one_failed_module() -> ProgramFacts {
    let mut base = facts(vec![], vec![]);
    base.scope.total_entries = 2;
    base.scope.selected_entries = 2;
    base.modules = vec![
        ModuleReport {
            debug_info: Some(true),
            ..report("a", ModuleAnalysis::Analyzed)
        },
        ModuleReport {
            diagnostic: Some("truncated".into()),
            ..report("b", ModuleAnalysis::Failed)
        },
    ];
    base
}

/// A function whose only mapped line is the given `(file, line)` pairs.
pub(crate) fn session_from_source_lines(lines: &[(&str, u32)]) -> Session {
    let mut only = function("m", "only", true, Linkage::Internal);
    only.mapped_lines = lines
        .iter()
        .map(|(file, line)| (PathBuf::from(file), *line))
        .collect();
    Session::new(facts(vec![only], Vec::new()), Vec::new())
}

/// Two ambiguous symbols, and one of them reachable through two different
/// declarations: `c1` and `c2` each define `caller` and declare `target`,
/// while `a` and `b` each define `target`. A second binding, `unused`, is
/// ambiguous too but sits on no path out of `caller`.
///
/// Distinguishes three counts that are easy to conflate: the bindings one
/// walk reached (one), the declarations it reached them through (two), and
/// every ambiguous binding in scope (two).
pub(crate) fn session_with_ambiguous_bindings() -> Session {
    let mut functions = Vec::new();
    let mut call_sites = Vec::new();
    for (index, module) in ["c1", "c2"].into_iter().enumerate() {
        let caller = function(module, "caller", true, Linkage::External);
        let declaration = function(module, "target", false, Linkage::External);
        call_sites.push(direct_call(&caller, &declaration, index as u32));
        functions.push(caller);
        functions.push(declaration);
    }
    functions.push(function("a", "target", true, Linkage::External));
    functions.push(function("b", "target", true, Linkage::External));

    let target = ambiguous_binding(
        "target",
        &["c1", "c2"],
        &[("a", Some("debug")), ("b", Some("release"))],
    );
    let unused = ambiguous_binding("unused", &["z"], &[("a", None), ("b", None)]);
    Session::new(facts(functions, call_sites), vec![target, unused])
}

/// One module the loader verified and extraction never saw, so
/// `analysis.verified` is the only non-zero count.
pub(crate) fn facts_with_one_verified_module() -> ProgramFacts {
    let mut base = facts(vec![], vec![]);
    base.scope.total_entries = 1;
    base.scope.selected_entries = 1;
    base.modules = vec![report("v", ModuleAnalysis::Verified)];
    base
}
