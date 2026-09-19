//! Cross-module symbol resolution, stated rather than assumed.
//!
//! The catalog does not record what the linker actually did. Joining two
//! modules by symbol name is therefore an assumption, and this module makes
//! it visible instead of turning it into an edge.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use super::facts::{FunctionFact, FunctionId, Linkage};

/// A symbol definition visible to other modules, from which cross-module
/// bindings may be formed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BindingCandidate {
    pub function: FunctionId,
    pub configuration_id: Option<String>,
}

/// The result of resolving a symbol across all modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    /// No definition visible to other modules.
    Unbound,
    /// Exactly one definition visible.
    Unique,
    /// Two or more definitions, possibly under different configurations.
    Ambiguous,
}

/// A declared symbol and the definitions that might satisfy it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SymbolBinding {
    pub symbol: String,
    pub declared_in: Vec<String>,
    pub candidates: Vec<BindingCandidate>,
    pub status: BindingStatus,
}

/// What one pass over the functions collects for a symbol. Each candidate
/// keeps its linkage, because that is what decides whether two definitions
/// of one symbol conflict or are the same function twice.
#[derive(Default)]
struct SymbolEntry {
    declared_in: Vec<String>,
    candidates: Vec<(Linkage, BindingCandidate)>,
}

/// Whether a definition can satisfy another module's declaration.
fn is_visible_definition(function: &FunctionFact) -> bool {
    match function.linkage {
        // Invisible to other modules.
        Linkage::Internal => false,
        // A body carried for inlining. No object file emits the symbol, so
        // it cannot be what another module's call resolves to.
        Linkage::AvailableExternally => false,
        _ => true,
    }
}

/// ODR duplicates of one symbol are one definition. C++ emits a copy of every
/// template instantiation, `inline` function and defaulted member into each
/// translation unit that used it, and the One Definition Rule makes those
/// copies the same function; the linker keeps one. Any copy therefore stands
/// for the rest.
///
/// Only candidates that are *all* ODR collapse. A plain `weak` definition
/// promises nothing about its copies, and an `external` definition beside an
/// ODR one is a real conflict -- both stay, and stay ambiguous.
fn collapse_odr_duplicates(
    mut candidates: Vec<(Linkage, BindingCandidate)>,
) -> Vec<BindingCandidate> {
    if candidates.len() > 1
        && candidates
            .iter()
            .all(|(linkage, _)| *linkage == Linkage::Odr)
    {
        candidates.truncate(1);
    }
    candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

/// Resolve symbols across all modules.
///
/// Returns one binding per symbol that is declared anywhere, including those
/// without candidates (Unbound). A definition that nothing references produces
/// no binding entry.
pub fn bind(
    functions: &[FunctionFact],
    configurations: &HashMap<String, Option<String>>,
) -> Vec<SymbolBinding> {
    let mut by_symbol: BTreeMap<&str, SymbolEntry> = BTreeMap::new();
    for function in functions {
        let entry = by_symbol.entry(&function.id.symbol).or_default();
        if !function.is_definition {
            entry.declared_in.push(function.id.module_id.clone());
        } else if is_visible_definition(function) {
            entry.candidates.push((
                function.linkage,
                BindingCandidate {
                    function: function.id.clone(),
                    configuration_id: configurations
                        .get(&function.id.module_id)
                        .cloned()
                        .flatten(),
                },
            ));
        }
    }

    by_symbol
        .into_iter()
        .filter(|(_, entry)| !entry.declared_in.is_empty())
        .map(|(symbol, entry)| {
            let candidates = collapse_odr_duplicates(entry.candidates);
            let status = match candidates.len() {
                0 => BindingStatus::Unbound,
                1 => BindingStatus::Unique,
                _ => BindingStatus::Ambiguous,
            };
            SymbolBinding {
                symbol: symbol.to_string(),
                declared_in: entry.declared_in,
                candidates,
                status,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::function;

    /// A declaration of `symbol` in `module`, as the shared builder spells it.
    fn declaration(module: &str, symbol: &str) -> FunctionFact {
        function(module, symbol, false, Linkage::External)
    }

    #[test]
    fn one_external_definition_binds_uniquely() {
        let functions = vec![
            function("a", "parse", true, Linkage::External),
            declaration("b", "parse"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        let binding = &bindings[0];
        assert_eq!(binding.status, BindingStatus::Unique);
        assert_eq!(
            binding.candidates[0].function,
            FunctionId {
                module_id: "a".to_string(),
                symbol: "parse".to_string(),
            },
            "candidate must be the definition from module a"
        );
        assert_eq!(
            binding.declared_in,
            vec!["b"],
            "declaration must be from module b"
        );
    }

    #[test]
    fn two_definitions_under_different_configurations_are_ambiguous() {
        let functions = vec![
            function("a", "parse", true, Linkage::External),
            function("b", "parse", true, Linkage::External),
            declaration("c", "parse"),
        ];
        let configurations = HashMap::from([
            ("a".to_string(), Some("debug".to_string())),
            ("b".to_string(), Some("release".to_string())),
        ]);
        let bindings = bind(&functions, &configurations);
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        let binding = &bindings[0];
        assert_eq!(binding.status, BindingStatus::Ambiguous);

        // Each candidate must keep its own module's configuration: swapping
        // the two would still give two candidates and the right status.
        for (module, configuration) in [("a", "debug"), ("b", "release")] {
            let candidate = binding
                .candidates
                .iter()
                .find(|c| c.function.module_id == module)
                .unwrap_or_else(|| panic!("candidate from module {module} must exist"));
            assert_eq!(
                candidate.configuration_id,
                Some(configuration.to_string()),
                "module {module} must be paired with config {configuration}"
            );
        }
    }

    #[test]
    fn internal_linkage_never_binds_across_modules() {
        let functions = vec![
            function("a", "helper", true, Linkage::Internal),
            declaration("b", "helper"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        assert_eq!(
            bindings[0].status,
            BindingStatus::Unbound,
            "a static definition cannot satisfy another module's declaration"
        );
        assert_eq!(
            bindings[0].candidates.len(),
            0,
            "internal linkage definition must not appear as a candidate"
        );
    }

    #[test]
    fn odr_copies_of_one_symbol_are_one_definition() {
        // What every C++ translation unit that instantiates a template or
        // uses an `inline` function emits: its own copy, `linkonce_odr`.
        let functions = vec![
            function("a", "twice", true, Linkage::Odr),
            function("b", "twice", true, Linkage::Odr),
            declaration("c", "twice"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        assert_eq!(
            bindings[0].status,
            BindingStatus::Unique,
            "ODR makes the copies the same function, so the symbol has one definition"
        );
        assert_eq!(
            bindings[0].candidates.len(),
            1,
            "a caller must be handed one definition, not a copy per module"
        );
    }

    #[test]
    fn two_weak_definitions_stay_ambiguous() {
        // `weak` without ODR is the override hook: the copies are allowed to
        // differ, so which one the linker kept is genuinely unknown here.
        let functions = vec![
            function("a", "pick", true, Linkage::Weak),
            function("b", "pick", true, Linkage::Weak),
            declaration("c", "pick"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        assert_eq!(bindings[0].status, BindingStatus::Ambiguous);
        assert_eq!(
            bindings[0].candidates.len(),
            2,
            "neither weak definition may be discarded"
        );
    }

    #[test]
    fn an_external_definition_beside_an_odr_copy_stays_ambiguous() {
        // Not a duplicate: one of these is a definition the ODR copies make
        // no promise about, so collapsing them would pick a side.
        let functions = vec![
            function("a", "twice", true, Linkage::Odr),
            function("b", "twice", true, Linkage::External),
            declaration("c", "twice"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        assert_eq!(bindings[0].status, BindingStatus::Ambiguous);
        assert_eq!(bindings[0].candidates.len(), 2);
    }

    #[test]
    fn an_available_externally_body_never_binds_across_modules() {
        // The body is there to be inlined; no object file emits the symbol.
        // Binding a call to it would name a definition that does not ship.
        let functions = vec![
            function("a", "helper", true, Linkage::AvailableExternally),
            declaration("b", "helper"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(bindings.len(), 1, "should have exactly one binding");
        assert_eq!(
            bindings[0].status,
            BindingStatus::Unbound,
            "a body kept for inlining cannot satisfy another module's declaration"
        );
        assert_eq!(bindings[0].candidates.len(), 0);
    }

    #[test]
    fn defined_symbol_with_no_declarations_produces_no_binding() {
        let functions = vec![
            function("a", "helper", true, Linkage::External),
            declaration("b", "other"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(
            bindings.len(),
            1,
            "should have exactly one binding (for 'other')"
        );
        assert_eq!(
            bindings[0].symbol, "other",
            "an unreferenced definition must produce no binding"
        );
    }
}
