//! Versioned descriptions of known modules and their evidence.

use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Error;

mod inventory;
pub use inventory::{inspect_bitcode, inventory};
mod selection;
pub use selection::{copy_modules, select_modules};

/// Location of a bitcode member stored inside an archive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveMember {
    pub index: usize,
    pub name: String,
}

/// Current on-disk catalog version. Readers reject unsupported versions.
pub const SCHEMA_VERSION: u32 = 1;
const CATALOG_KIND: &str = "rllvm-module-catalog";

/// Origin of the inventory, distinct from a verified historical build.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogOrigin {
    pub kind: String,
    pub input: PathBuf,
    pub sha256: Option<String>,
}

/// OR within each field, AND between nonempty fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModuleSelection {
    pub module_ids: Vec<String>,
    pub sources: Vec<PathBuf>,
    pub configuration_ids: Vec<String>,
}

/// Selected evidence and its boundaries; available modules do not prove completeness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogScope {
    pub kind: String,
    pub total_entries: usize,
    pub selected_entries: usize,
    pub selection: ModuleSelection,
    /// Earlier filters when an already selected catalog is narrowed again.
    #[serde(default)]
    pub selection_history: Vec<ModuleSelection>,
    /// Explicit analysis overrides, retained even if every entry failed.
    #[serde(default)]
    pub analysis_arguments: Vec<String>,
    pub whole_program_complete: Option<bool>,
    pub limitations: Vec<String>,
}

/// A source association with an explicit origin (IR, debug information, or database).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceAssociation {
    pub path: PathBuf,
    pub directory: Option<PathBuf>,
    pub origin: String,
    /// Hash of an observed current source file, not its dependency closure.
    pub content_sha256: Option<String>,
}

/// Identity of the compiler used for a recorded analysis compilation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerIdentity {
    pub path: PathBuf,
    pub realpath: Option<PathBuf>,
    pub version: String,
    pub sha256: Option<String>,
}

/// Original command and the explicitly different analysis invocation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilationRecord {
    pub entry_index: usize,
    pub directory: PathBuf,
    pub recorded_arguments: Vec<String>,
    pub recorded_output: Option<PathBuf>,
    pub effective_arguments: Vec<String>,
    pub analysis_id: String,
    pub extra_arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub environment_complete: bool,
}

/// Availability of a catalog entry. Missing entries are retained in inventories.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleStatus {
    #[default]
    Planned,
    Available,
    Missing,
    Unsupported,
    Failed,
}

/// Module identity is separate from its content and compilation configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub id: String,
    /// Absolute, or relative to the directory containing this catalog.
    pub path: Option<PathBuf>,
    pub recorded_path: Option<PathBuf>,
    #[serde(default)]
    pub archive_member: Option<ArchiveMember>,
    pub content_sha256: Option<String>,
    pub sources: Vec<SourceAssociation>,
    pub target_triple: Option<String>,
    pub data_layout: Option<String>,
    pub compiler: Option<CompilerIdentity>,
    pub configuration_id: Option<String>,
    pub build_identity: Option<String>,
    pub source_snapshot: Option<String>,
    pub ir_stage: Option<String>,
    pub debug_info: Option<bool>,
    pub compilation: Option<CompilationRecord>,
    pub status: ModuleStatus,
    /// Optional diagnostic file, relative to the catalog or absolute.
    pub diagnostic_path: Option<PathBuf>,
    pub diagnostics: Vec<String>,
    pub unavailable_metadata: Vec<String>,
}

impl ModuleRecord {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }
}

/// Portable catalog shared by recorded-artifact inventory and database import.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleCatalog {
    pub schema_version: u32,
    pub kind: String,
    pub origin: CatalogOrigin,
    pub scope: CatalogScope,
    pub modules: Vec<ModuleRecord>,
}

impl ModuleCatalog {
    pub fn new(origin: CatalogOrigin, scope_kind: &str, modules: Vec<ModuleRecord>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: CATALOG_KIND.into(),
            origin,
            scope: CatalogScope {
                kind: scope_kind.into(),
                total_entries: modules.len(),
                selected_entries: modules.len(),
                selection: ModuleSelection::default(),
                selection_history: Vec::new(),
                analysis_arguments: Vec::new(),
                whole_program_complete: None,
                limitations: vec![
                    "Module availability does not establish whole-program completeness.".into(),
                ],
            },
            modules,
        }
    }

    /// Validate the format without resolving or reading module paths.
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != SCHEMA_VERSION || self.kind != CATALOG_KIND {
            return Err(Error::InvalidArguments(format!(
                "unsupported catalog kind/version: {} / {}",
                self.kind, self.schema_version
            )));
        }
        if self.scope.selected_entries != self.modules.len()
            || self.scope.total_entries < self.scope.selected_entries
        {
            return Err(Error::InvalidArguments(
                "inconsistent catalog selection counts".into(),
            ));
        }
        let mut ids = HashSet::new();
        for module in &self.modules {
            if module.id.is_empty() || !ids.insert(&module.id) {
                return Err(Error::InvalidArguments(
                    "empty or duplicate module id".into(),
                ));
            }
            if module.status == ModuleStatus::Available
                && (module.path.is_none() || module.content_sha256.is_none())
            {
                return Err(Error::InvalidArguments(format!(
                    "available module {} lacks path/hash",
                    module.id
                )));
            }
            if let Some(hash) = &module.content_sha256
                && (hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(Error::InvalidArguments(format!(
                    "invalid module hash: {}",
                    module.id
                )));
            }
        }
        Ok(())
    }
}

/// Read and validate a catalog; resolving its relative paths is a separate operation.
pub fn read_catalog(path: &Path) -> Result<ModuleCatalog, Error> {
    let catalog: ModuleCatalog = serde_json::from_reader(File::open(path)?)
        .map_err(|error| Error::InvalidArguments(format!("invalid catalog JSON: {error}")))?;
    catalog.validate()?;
    Ok(catalog)
}

/// Atomically publish a catalog without replacing an existing destination.
pub fn write_catalog(path: &Path, catalog: &ModuleCatalog) -> Result<(), Error> {
    catalog.validate()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, catalog)
        .map_err(|error| Error::InvalidArguments(format!("cannot serialize catalog: {error}")))?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

/// Stable SHA-256 content digest.
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Hash large modules/compilers without loading the file into memory.
pub fn hash_file(path: &Path) -> Result<String, Error> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Hash length-delimited identity components without ambiguous concatenation.
pub fn identity(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_has_a_stable_external_representation() {
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(identity(&["a", "bc"]), identity(&["ab", "c"]));
    }

    #[test]
    fn catalog_rejects_unknown_versions_and_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let mut catalog = ModuleCatalog::new(
            CatalogOrigin {
                kind: "artifact".into(),
                input: "app".into(),
                sha256: None,
            },
            "recorded_modules",
            vec![ModuleRecord::new("first")],
        );
        catalog.schema_version = 99;
        std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        assert!(
            read_catalog(&path)
                .unwrap_err()
                .to_string()
                .contains("version")
        );
        catalog.schema_version = 1;
        catalog.modules.push(ModuleRecord::new("first"));
        catalog.scope.total_entries = 2;
        catalog.scope.selected_entries = 2;
        std::fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        assert!(
            read_catalog(&path)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn catalog_publication_preserves_existing_files_and_unknown_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let catalog = ModuleCatalog::new(
            CatalogOrigin {
                kind: "artifact".into(),
                input: "app".into(),
                sha256: None,
            },
            "recorded_modules",
            vec![ModuleRecord::new("first")],
        );
        write_catalog(&path, &catalog).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["modules"][0]["compiler"].is_null());
        assert!(value["modules"][0]["configuration_id"].is_null());
        assert!(value["scope"]["whole_program_complete"].is_null());
        assert_eq!(read_catalog(&path).unwrap().modules[0].id, "first");
        assert!(write_catalog(&path, &catalog).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
