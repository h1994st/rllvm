//! Cross-module symbol resolution, stated rather than assumed.
//!
//! The catalog does not record what the linker actually did. Joining two
//! modules by symbol name is therefore an assumption, and this module makes
//! it visible instead of turning it into an edge.

use std::collections::{BTreeMap, HashMap};

use super::facts::{FunctionFact, FunctionId, Linkage};

/// A symbol definition visible to other modules, from which cross-module
/// bindings may be formed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingCandidate {
    pub function: FunctionId,
    pub configuration_id: Option<String>,
}

/// The result of resolving a symbol across all modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingStatus {
    /// No definition visible to other modules.
    Unbound,
    /// Exactly one definition visible.
    Unique,
    /// Two or more definitions, possibly under different configurations.
    Ambiguous,
}

/// A declared symbol and the definitions that might satisfy it.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    use std::collections::BTreeSet;

    use super::*;

    /// Helper to construct a function definition with the given module, symbol,
    /// and linkage.
    fn definition(module_id: &str, symbol: &str, linkage: Linkage) -> FunctionFact {
        FunctionFact {
            id: FunctionId {
                module_id: module_id.to_string(),
                symbol: symbol.to_string(),
            },
            is_definition: true,
            linkage,
            signature: "void()".to_string(),
            location: None,
            mapped_lines: BTreeSet::new(),
        }
    }

    /// Helper to construct a function declaration with the given module and
    /// symbol.
    fn declaration(module_id: &str, symbol: &str) -> FunctionFact {
        FunctionFact {
            id: FunctionId {
                module_id: module_id.to_string(),
                symbol: symbol.to_string(),
            },
            is_definition: false,
            linkage: Linkage::External,
            signature: "void()".to_string(),
            location: None,
            mapped_lines: BTreeSet::new(),
        }
    }

    #[test]
    fn one_external_definition_binds_uniquely() {
        let functions = vec![
            definition("a", "parse", Linkage::External),
            declaration("b", "parse"),
        ];
        let bindings = bind(&functions, &Default::default());
        let binding = &bindings[0];
        assert_eq!(binding.status, BindingStatus::Unique);
        assert_eq!(binding.candidates.len(), 1);
    }

    #[test]
    fn two_definitions_under_different_configurations_are_ambiguous() {
        let functions = vec![
            definition("a", "parse", Linkage::External),
            definition("b", "parse", Linkage::External),
            declaration("c", "parse"),
        ];
        let mut configurations = HashMap::new();
        configurations.insert("a".to_string(), Some("debug".to_string()));
        configurations.insert("b".to_string(), Some("release".to_string()));
        let bindings = bind(&functions, &configurations);
        assert_eq!(bindings[0].status, BindingStatus::Ambiguous);
        assert_eq!(bindings[0].candidates.len(), 2);
    }

    #[test]
    fn internal_linkage_never_binds_across_modules() {
        let functions = vec![
            definition("a", "helper", Linkage::Internal),
            declaration("b", "helper"),
        ];
        let bindings = bind(&functions, &Default::default());
        assert_eq!(
            bindings[0].status,
            BindingStatus::Unbound,
            "a static definition cannot satisfy another module's declaration"
        );
    }
}
