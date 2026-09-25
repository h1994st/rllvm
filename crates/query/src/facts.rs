//! Owned facts extracted from bitcode. No LLVM handles, no I/O.

use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use rllvm_core::catalog::{CatalogOrigin, CatalogScope, DigestOrigin};

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
    /// Current file hashes to the recorded digest. Read with `status_basis`:
    /// only a `compiler` or `capture` digest makes this a statement about
    /// the bitcode rather than about the catalog.
    Current,
    /// File exists and differs; recorded line numbers may no longer apply.
    Modified,
    Missing,
    /// No digest was recorded, so staleness cannot be determined.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub directory: Option<PathBuf>,
    pub line: u32,
    pub column: u32,
    pub source_status: SourceStatus,
    /// What `source_status` was decided against. `compiler` and `capture`
    /// were both taken when the module was built, so `current` proves the
    /// source still matches the bitcode. `inventory` was taken when the
    /// catalog was written, which is after the build: `current` there proves
    /// only that nothing changed since. Absent when there was no digest to
    /// check, or the file is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_basis: Option<DigestOrigin>,
    /// Innermost-first chain of inlining frames; empty when not inlined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inlined_at: Vec<SourceLocation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Linkage {
    External,
    Internal,
    /// `linkonce_odr` and `weak_odr`: one language entity, emitted into every
    /// translation unit that used it. The One Definition Rule makes the
    /// copies the same function, so they are one definition, not a conflict.
    Odr,
    /// `weak` and `linkonce` without ODR. Replaceable by design, and the
    /// copies may genuinely differ.
    Weak,
    /// `available_externally`: a body carried for inlining that no object
    /// file emits, so it cannot satisfy another module's declaration.
    AvailableExternally,
    Other,
}

/// The language a function was written in, as far as the bitcode says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    /// Any language other than Rust. Not refined further: `ffi-exports`, the
    /// only reader, asks whether a function is Rust and nothing more.
    Other,
}

/// Where a [`SourceLanguage`] came from, because the two prove different
/// things: debug info names the function's own compile unit, while the
/// producer speaks for the whole module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageBasis {
    /// The `DICompileUnit` the function's subprogram belongs to.
    DebugInfo,
    /// The module's `!llvm.ident`, when every entry agrees.
    Producer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLanguage {
    pub name: Language,
    pub basis: LanguageBasis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionFact {
    pub id: FunctionId,
    pub is_definition: bool,
    pub linkage: Linkage,
    /// `None` for a declaration, which is not written in the module that
    /// declares it, and when neither debug info nor the module's producer
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<SourceLanguage>,
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
    /// Quoted from the module's `!llvm.ident`, in order. Empty until
    /// extraction reads it, and for a module whose producer wrote none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producers: Vec<String>,
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
