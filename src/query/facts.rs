//! Owned facts extracted from bitcode. No LLVM handles, no I/O.

use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::{CatalogOrigin, CatalogScope};

/// A function is identified by module and symbol, never by symbol alone: the
/// catalog preserves separate compilations of one source on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FunctionId {
    pub module_id: String,
    pub symbol: String,
}

/// Distinct for two calls on the same source line.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CallSiteId {
    pub function: FunctionId,
    pub block_index: u32,
    pub instruction_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Current file hashes to the value recorded at capture.
    Current,
    /// File exists and differs; recorded line numbers may no longer apply.
    Modified,
    Missing,
    /// No hash was recorded, so staleness cannot be determined.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub directory: Option<PathBuf>,
    pub line: u32,
    pub column: u32,
    pub source_status: SourceStatus,
    /// Innermost-first chain of inlining frames; empty when not inlined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inlined_at: Vec<SourceLocation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Linkage {
    External,
    Internal,
    Weak,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionFact {
    pub id: FunctionId,
    pub is_definition: bool,
    pub linkage: Linkage,
    pub signature: String,
    pub location: Option<SourceLocation>,
    /// Lines any instruction in this function maps to. Supports `at`; this is
    /// not source-range containment.
    pub mapped_lines: BTreeSet<(PathBuf, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallTarget {
    /// The callee as referenced in this module. Never rebound by symbol name.
    Direct {
        callee: FunctionId,
    },
    Indirect {
        signature: String,
        /// Upper bound from `!callees`: a defined execution cannot call
        /// outside this set. Not a claim that these targets are reachable.
        #[serde(skip_serializing_if = "Option::is_none")]
        llvm_target_bound: Option<Vec<FunctionId>>,
    },
    Intrinsic {
        name: String,
    },
    InlineAsm,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallSiteFact {
    pub id: CallSiteId,
    pub location: Option<SourceLocation>,
    pub target: CallTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseKind {
    StoredToMemory,
    PassedAsArgument,
    GlobalInitializer,
    ReturnedValue,
    Other,
}

/// A use of a function that is not a call in callee position.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UseFact {
    pub used: FunctionId,
    pub in_function: Option<FunctionId>,
    pub location: Option<SourceLocation>,
    pub kind: UseKind,
}

/// Mirrors the catalog's `ModuleStatus` where it applies, and adds what only
/// this pipeline knows. Every recorded status keeps its own meaning: folding
/// them together throws away the reason the capture already recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleAnalysis {
    /// Read and hash-verified, but extraction has not run yet.
    Verified,
    /// Parsed successfully. Only extraction may set this.
    Analyzed,
    /// Bytes on disk do not match the recorded content hash.
    Changed,
    /// The file is not there.
    Missing,
    /// Present but unreadable, unparseable, or rejected by LLVM.
    Failed,
    /// The catalog recorded it as unsupported at capture time.
    Unsupported,
    /// The catalog planned it and it was never produced.
    NotBuilt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleReport {
    pub id: String,
    pub status: ModuleAnalysis,
    pub ir_stage: Option<String>,
    pub debug_info: Option<bool>,
    pub compiler: Option<String>,
    /// Retained from the catalog so answers can distinguish separate
    /// compilations of one source without rebuilding a provenance model.
    pub configuration_id: Option<String>,
    pub content_sha256: Option<String>,
    pub target_triple: Option<String>,
    /// The capture's recorded reason, or the failure observed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgramFacts {
    pub functions: Vec<FunctionFact>,
    pub call_sites: Vec<CallSiteFact>,
    pub uses: Vec<UseFact>,
    /// Quoted from the catalog; never recomputed and never shrunk.
    pub scope: CatalogScope,
    /// Quoted from the catalog, so `provenance.catalog_origin` in the
    /// envelope is reported rather than reconstructed.
    pub origin: CatalogOrigin,
    pub modules: Vec<ModuleReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_sites_on_one_line_have_distinct_identities() {
        let function = FunctionId {
            module_id: "m".into(),
            symbol: "caller".into(),
        };
        let first = CallSiteId {
            function: function.clone(),
            block_index: 0,
            instruction_index: 3,
        };
        let second = CallSiteId {
            function,
            block_index: 0,
            instruction_index: 7,
        };
        assert_ne!(first, second);
    }

    #[test]
    fn a_bare_symbol_is_not_a_function_identity() {
        let left = FunctionId {
            module_id: "a".into(),
            symbol: "f".into(),
        };
        let right = FunctionId {
            module_id: "b".into(),
            symbol: "f".into(),
        };
        assert_ne!(left, right, "same symbol in two modules must not collide");
    }
}
