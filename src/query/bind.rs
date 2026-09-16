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

/// Resolve symbols across all modules.
///
/// Returns one binding per symbol that is declared anywhere, including those
/// without candidates (Unbound). A definition that nothing references produces
/// no binding entry.
pub fn bind(
    functions: &[FunctionFact],
    configurations: &HashMap<String, Option<String>>,
) -> Vec<SymbolBinding> {
    let mut by_symbol: BTreeMap<&str, (Vec<String>, Vec<BindingCandidate>)> = BTreeMap::new();
    for function in functions {
        let entry = by_symbol.entry(&function.id.symbol).or_default();
        if function.is_definition {
            // Internal linkage is invisible to other modules.
            if function.linkage != Linkage::Internal {
                entry.1.push(BindingCandidate {
                    function: function.id.clone(),
                    configuration_id: configurations
                        .get(&function.id.module_id)
                        .cloned()
                        .flatten(),
                });
            }
        } else {
            entry.0.push(function.id.module_id.clone());
        }
    }

    by_symbol
        .into_iter()
        .filter(|(_, (declared_in, _))| !declared_in.is_empty())
        .map(|(symbol, (declared_in, candidates))| {
            let status = match candidates.len() {
                0 => BindingStatus::Unbound,
                1 => BindingStatus::Unique,
                _ => BindingStatus::Ambiguous,
            };
            SymbolBinding {
                symbol: symbol.to_string(),
                declared_in,
                candidates,
                status,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::testing::function;

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
