//! Resolve, read and verify the modules a catalog names.
//!
//! `read_catalog` validates structure only. Everything here is about what is
//! actually on disk now, which is not what the catalog remembers.

use std::{
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
};

use rllvm_core::{
    catalog::{
        ArchiveCache, ArchiveMember, CatalogOrigin, CatalogScope, DigestOrigin, ModuleCatalog,
        ModuleRecord, ModuleStatus, hash_bytes, read_catalog,
    },
    error::Error,
};

use crate::facts::{ModuleAnalysis, ModuleReport, SourceStatus};

pub struct LoadedModule {
    pub id: String,
    pub bytes: Vec<u8>,
    /// The catalog record, retained whole. Provenance is not re-modelled:
    /// configuration id, content hash, target triple, compiler identity and
    /// compilation record are needed by later answers and are already here.
    pub record: ModuleRecord,
}

pub struct Loaded {
    /// Verified modules, yielded one at a time so buffers are not all
    /// resident at once. See `for_each_module`.
    pub pending: Vec<PendingModule>,
    pub reports: Vec<ModuleReport>,
    pub scope: CatalogScope,
    pub origin: CatalogOrigin,
    /// Keyed by module as well as path: two modules may record different
    /// hashes for one source, and a path-only key lets the last one read
    /// overwrite the status of every other. This records what was observed
    /// at load time and is not refreshed while answering queries.
    pub source_status: HashMap<(String, PathBuf), SourceState>,
}

/// One source's freshness, and the digest origin it was decided against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceState {
    pub status: SourceStatus,
    pub basis: Option<DigestOrigin>,
}

/// A verified module whose bytes have not been read yet.
pub struct PendingModule {
    pub id: String,
    pub path: PathBuf,
    pub member: Option<ArchiveMember>,
    pub record: ModuleRecord,
}

/// Relative module paths resolve against the directory holding the catalog,
/// per the contract documented at `catalog.rs:113`.
fn resolve(catalog_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        catalog_dir.join(path)
    }
}

/// Reads the catalog at `path` and loads what it names, resolving relative
/// module paths against the directory holding it.
pub fn load_catalog(path: &Path) -> Result<Loaded, Error> {
    let catalog: ModuleCatalog = read_catalog(path)?;
    let catalog_dir = path.parent().unwrap_or(Path::new("."));
    load_catalog_value(catalog, catalog_dir)
}

/// The same load for a catalog already in memory, so a caller that just
/// built one -- `catalog::inventory` over an artifact -- need not write it
/// to disk and read it back to query it.
pub fn load_catalog_value(catalog: ModuleCatalog, catalog_dir: &Path) -> Result<Loaded, Error> {
    let catalog_dir = catalog_dir.to_path_buf();

    let mut pending = Vec::new();
    let mut reports = Vec::new();
    let mut source_status: HashMap<(String, PathBuf), SourceState> = HashMap::new();
    let mut archives = ArchiveCache::default();

    for record in &catalog.modules {
        let report_base = |status, diagnostic| ModuleReport {
            id: record.id.clone(),
            status,
            ir_stage: record.ir_stage.clone(),
            debug_info: record.debug_info,
            compiler: record.compiler.as_ref().map(|c| c.version.clone()),
            configuration_id: record.configuration_id.clone(),
            content_sha256: record.content_sha256.clone(),
            target_triple: record.target_triple.clone(),
            diagnostic,
        };

        // Each recorded status keeps its own meaning, and its recorded
        // diagnostics travel with it. Collapsing all of them to Missing
        // discards the reason the capture already knew. `Available` maps to
        // `None`: it is analysed below rather than reported from its
        // catalog status alone, so there is no arm left to panic on.
        let unavailable = match record.status {
            ModuleStatus::Missing => Some(ModuleAnalysis::Missing),
            ModuleStatus::Failed => Some(ModuleAnalysis::Failed),
            ModuleStatus::Unsupported => Some(ModuleAnalysis::Unsupported),
            ModuleStatus::Planned => Some(ModuleAnalysis::NotBuilt),
            ModuleStatus::Available => None,
        };
        if let Some(status) = unavailable {
            let recorded = (!record.diagnostics.is_empty()).then(|| record.diagnostics.join("; "));
            reports.push(report_base(status, recorded));
            continue;
        }

        let Some(path) = record.path.as_ref().map(|p| resolve(&catalog_dir, p)) else {
            reports.push(report_base(ModuleAnalysis::Missing, None));
            continue;
        };

        // Archive members reuse the extraction helper the inventory already
        // uses; a plain path is read directly. A file that is absent is
        // Missing; a file that exists but cannot be read is Failed, and the
        // two are not the same fact about the capture.
        let bytes = match &record.archive_member {
            Some(member) => match archives.module(&path, member).map(<[u8]>::to_vec) {
                Ok(bytes) => bytes,
                // `ArchiveData::read` opens the archive itself via `fs::metadata`/
                // `fs::read`, and `Error::Io` preserves the wrapped `io::Error`'s
                // kind: an absent archive surfaces the same `NotFound` an absent
                // plain module file would, so it gets the same Missing treatment.
                Err(Error::Io(io_error)) if io_error.kind() == std::io::ErrorKind::NotFound => {
                    reports.push(report_base(
                        ModuleAnalysis::Missing,
                        Some(io_error.to_string()),
                    ));
                    continue;
                }
                Err(error) => {
                    reports.push(report_base(ModuleAnalysis::Failed, Some(error.to_string())));
                    continue;
                }
            },
            None => match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    reports.push(report_base(
                        ModuleAnalysis::Missing,
                        Some(error.to_string()),
                    ));
                    continue;
                }
                Err(error) => {
                    reports.push(report_base(ModuleAnalysis::Failed, Some(error.to_string())));
                    continue;
                }
            },
        };

        // Verify against the bytes actually read, not against the record.
        if let Some(recorded) = &record.content_sha256
            && hash_bytes(&bytes) != *recorded
        {
            reports.push(report_base(ModuleAnalysis::Changed, None));
            continue;
        }

        for association in &record.sources {
            let source = association.resolved_path();
            let state = match (&association.digest, std::fs::read(&source)) {
                (_, Err(_)) => SourceState {
                    status: SourceStatus::Missing,
                    basis: None,
                },
                (None, Ok(_)) => SourceState {
                    status: SourceStatus::Unknown,
                    basis: None,
                },
                (Some(digest), Ok(current)) => SourceState {
                    status: if digest.matches(&current) {
                        SourceStatus::Current
                    } else {
                        SourceStatus::Modified
                    },
                    // Carried through so an answer can say what `current`
                    // was decided against: an inventory digest was taken
                    // after the build and proves less than a compiler one.
                    basis: Some(digest.origin),
                },
            };
            // Two associations can name one file, and only one of them may
            // carry a digest. Keep whichever actually had something to check
            // so the status does not depend on the order they were recorded.
            match source_status.entry((record.id.clone(), source)) {
                Entry::Occupied(mut slot) if slot.get().basis.is_none() => {
                    slot.insert(state);
                }
                Entry::Occupied(_) => {}
                Entry::Vacant(slot) => {
                    slot.insert(state);
                }
            }
        }

        // Verified, not yet analysed: extraction decides that, and it has not
        // run. Recording Analyzed here would claim a parse that never happened.
        drop(bytes);
        reports.push(report_base(ModuleAnalysis::Verified, None));
        pending.push(PendingModule {
            id: record.id.clone(),
            path,
            member: record.archive_member.clone(),
            record: record.clone(),
        });
    }

    Ok(Loaded {
        pending,
        reports,
        // Quoted verbatim: a parse failure must not narrow the claimed program.
        scope: catalog.scope.clone(),
        origin: catalog.origin.clone(),
        source_status,
    })
}

/// Reads and hands over one module at a time, so only one bitcode buffer is
/// resident. The archive cache is dropped when the loop ends, before any
/// session begins serving requests.
///
/// A module that verified at load time but cannot be read now -- deleted or
/// rewritten by a concurrent build in between -- is returned in the
/// unreadable list and skipped, not propagated. `load_catalog` already
/// records an unreadable module and carries on with the rest, and one
/// module vanishing must not cost the answer every other module. Only an
/// error raised by `visit` itself aborts the walk.
pub fn for_each_module(
    loaded: &Loaded,
    mut visit: impl FnMut(LoadedModule) -> Result<(), Error>,
) -> Result<Vec<(String, Error)>, Error> {
    let mut archives = ArchiveCache::default();
    let mut unreadable = Vec::new();
    for pending in &loaded.pending {
        let bytes = match &pending.member {
            Some(member) => archives.module(&pending.path, member).map(<[u8]>::to_vec),
            None => std::fs::read(&pending.path).map_err(Error::from),
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                unreadable.push((pending.id.clone(), error));
                continue;
            }
        };
        visit(LoadedModule {
            id: pending.id.clone(),
            bytes,
            record: pending.record.clone(),
        })?;
    }
    Ok(unreadable)
}
