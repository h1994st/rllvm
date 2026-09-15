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
            CallSiteFact, CallSiteId, CallTarget, FunctionFact, FunctionId, Linkage, ProgramFacts,
            SourceLocation, SourceStatus, UseFact, UseKind,
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
    let site = CallSiteFact {
        id: CallSiteId {
            function: a.id.clone(),
            block_index: 0,
            instruction_index: 0,
        },
        location: None,
        target: CallTarget::Indirect {
            signature: "i32 (i32, i32)".into(),
            llvm_target_bound: Some(vec![target.id.clone(), other.id.clone()]),
        },
    };
    Session::new(facts(vec![a, target, other], vec![site]), Vec::new())
}

/// `caller` calls a symbol two modules define under different configurations.
pub(crate) fn session_with_ambiguous_binding() -> Session {
    let caller = function("c", "caller", true, Linkage::External);
    let declaration = function("c", "target", false, Linkage::External);
    let first = function("a", "target", true, Linkage::External);
    let second = function("b", "target", true, Linkage::External);
    let call = direct_call(&caller, &declaration, 0);
    let functions = vec![caller, declaration, first.clone(), second.clone()];

    let binding = SymbolBinding {
        symbol: "target".into(),
        declared_in: vec!["c".into()],
        candidates: vec![
            BindingCandidate {
                function: first.id,
                configuration_id: Some("debug".into()),
            },
            BindingCandidate {
                function: second.id,
                configuration_id: Some("release".into()),
            },
        ],
        status: BindingStatus::Ambiguous,
    };
    Session::new(facts(functions, vec![call]), vec![binding])
}

/// `a` calls `b`; `b` reaches `c` only through an indirect call CVP could not
/// bound, so no path exists over direct edges.
pub(crate) fn session_with_indirect_gap() -> Session {
    let a = function("m", "a", true, Linkage::Internal);
    let b = function("m", "b", true, Linkage::Internal);
    let c = function("m", "c", true, Linkage::Internal);
    let call = direct_call(&a, &b, 0);
    let indirect = CallSiteFact {
        id: CallSiteId {
            function: b.id.clone(),
            block_index: 0,
            instruction_index: 1,
        },
        location: None,
        target: CallTarget::Indirect {
            signature: "i32 (i32, i32)".into(),
            llvm_target_bound: None,
        },
    };
    Session::new(facts(vec![a, b, c], vec![call, indirect]), Vec::new())
}

/// One indirect site at `t.c:4`, and an `add` whose address is taken.
pub(crate) fn session_with_address_taken_function() -> Session {
    let add = function("m", "add", true, Linkage::Internal);
    let caller = function("m", "caller", true, Linkage::Internal);
    let mut base = facts(
        vec![add.clone(), caller.clone()],
        vec![CallSiteFact {
            id: CallSiteId {
                function: caller.id.clone(),
                block_index: 0,
                instruction_index: 0,
            },
            location: Some(SourceLocation {
                file: "t.c".into(),
                directory: None,
                line: 4,
                column: 3,
                source_status: SourceStatus::Unknown,
                inlined_at: Vec::new(),
            }),
            target: CallTarget::Indirect {
                signature: "i32 (i32, i32)".into(),
                llvm_target_bound: None,
            },
        }],
    );
    base.uses = vec![UseFact {
        used: add.id,
        in_function: Some(caller.id),
        location: None,
        kind: UseKind::StoredToMemory,
    }];
    Session::new(base, Vec::new())
}
