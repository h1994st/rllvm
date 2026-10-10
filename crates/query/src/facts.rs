//! Owned facts extracted from bitcode. No LLVM handles, no I/O.
//!
//! Changing what these types carry requires bumping `FACTS_FORMAT` in
//! `cache.rs`: `the_facts_format_names_what_extraction_produces` says how.

use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use rllvm_core::{
    catalog::{CatalogOrigin, CatalogScope, DigestOrigin},
    error::Error,
};

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
    /// For an alias, the function in the same module it stands for. An alias
    /// is a definition with no body of its own: calls to it run the target,
    /// so queries follow it there. `None` for every ordinary function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<FunctionId>,
}

/// A field of a record type, as the IR can prove it: the record's source
/// name and the field's byte offset within it. The identity two facts are
/// joined on; a member name, when known, is carried beside it, not in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldRef {
    /// Normalized: `ops`, `ns::Foo`. Never empty.
    pub record: String,
    pub offset: u64,
}

impl std::fmt::Display for FieldRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.record, self.offset)
    }
}

impl std::str::FromStr for FieldRef {
    type Err = Error;

    /// Splits on the last `@`, so a record name containing one survives.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            Error::InvalidArguments(format!(
                "field `{text}` is not `record@offset`, such as `ops@8`"
            ))
        };
        let (record, offset) = text.rsplit_once('@').ok_or_else(invalid)?;
        if record.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            record: record.to_string(),
            offset: offset.parse().map_err(|_| invalid())?,
        })
    }
}

/// Which IR evidence named the field. Kept because they prove the same
/// thing at different optimization levels, and a reader debugging a missing
/// join needs to know which one spoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldBasis {
    /// A struct-typed `getelementptr` with constant indices (-O0).
    StructGep,
    /// A `!tbaa` struct-path access tag (-O1 and above).
    Tbaa,
    /// The position inside a named struct constant of a global initializer.
    Initializer,
    /// A global initializer of literal type, named by the global's debug-info type.
    DebugInfo,
}

/// Where a function pointer is kept on its way from the function to a call:
/// a global variable, or a parameter of a function. Unlike a field, a slot is
/// one storage location, so the functions that flow into it are exactly what
/// a call through it can target, within the captured program.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Slot {
    /// `module_id` is set only for a global other modules cannot name.
    Global {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        module_id: Option<String>,
        name: String,
    },
    /// The function as referenced where the flow was seen: a declaration in
    /// a caller's module binds to its definition when solved.
    Param { function: FunctionId, index: u32 },
    /// The location whose address a function returns, as an accessor such
    /// as libxml2's `__xmlGenericError()` does. Loads and stores through the
    /// result reach whatever the function returns the address of.
    Returned { function: FunctionId },
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Slot::Global { name, .. } => write!(f, "global {name}"),
            Slot::Param { function, index } => write!(f, "param {index} of {}", function.symbol),
            Slot::Returned { function } => write!(f, "*{}()", function.symbol),
        }
    }
}

/// A function returns the address of a global or of a record field. Exactly
/// one of `global` and `field` is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnedAddress {
    pub function: FunctionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldEvidence>,
}

/// A pointer read from one slot is stored or passed into another: a
/// parameter passed on as an argument, or stored into a global.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFlow {
    pub from: Slot,
    pub into: Slot,
}

/// A pointer read from a slot is stored into a record field, as a setter such
/// as `set_cb(obj, cb)` does with its parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldFlow {
    pub from: Slot,
    pub into: FieldEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldEvidence {
    pub field: FieldRef,
    pub basis: FieldBasis,
    /// The source member name at that offset, from debug info. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
        /// The record field the called pointer was loaded from, when the IR
        /// proves one. A plain pointer variable or a vtable slot has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_field: Option<FieldEvidence>,
        /// The global or parameter the called pointer was read from, when no
        /// field names it. Traced through `-O0` stack slots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_slot: Option<Slot>,
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
    /// The global whose initializer holds the address, directly or inside an
    /// aggregate such as a dispatch table.
    pub in_global: Option<String>,
    pub location: Option<SourceLocation>,
    pub kind: UseKind,
    /// The record field the address goes into: a store's destination, or
    /// the slot an initializer puts it in. Only ever set for
    /// `StoredToMemory` and `GlobalInitializer`, and only when the IR proves
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldEvidence>,
    /// The global the address is stored into or initializes directly, or the
    /// parameter it is passed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into_slot: Option<Slot>,
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
    pub slot_flows: Vec<SlotFlow>,
    pub field_flows: Vec<FieldFlow>,
    pub returned_addresses: Vec<ReturnedAddress>,
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

    #[test]
    fn a_field_reads_back_from_its_display() {
        let field = FieldRef {
            record: "n::S".into(),
            offset: 0,
        };
        assert_eq!(field.to_string(), "n::S@0");
        assert_eq!(field.to_string().parse::<FieldRef>().unwrap(), field);

        assert_eq!(
            "a@b@8".parse::<FieldRef>().unwrap(),
            FieldRef {
                record: "a@b".into(),
                offset: 8
            },
            "the offset follows the last `@`"
        );
        for invalid in ["ops@", "@8", "ops", "ops@-8", "ops@x"] {
            assert!(
                matches!(invalid.parse::<FieldRef>(), Err(Error::InvalidArguments(_))),
                "{invalid}"
            );
        }
    }
}
