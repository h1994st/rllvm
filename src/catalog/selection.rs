//! Selection by known metadata and publication of portable, unmerged modules.

use std::{collections::BTreeMap, fs, io::Write, path::Path};

use super::{
    ModuleCatalog, ModuleRecord, ModuleSelection, ModuleStatus, hash_bytes, hash_file,
    write_catalog,
};
use crate::error::Error;

fn source_matches(module: &ModuleRecord, source: &Path, cwd: &Path) -> bool {
    let requested = if source.is_absolute() {
        source.to_path_buf()
    } else {
        cwd.join(source)
    };
    module.sources.iter().any(|known| {
        let path = if known.path.is_absolute() {
            known.path.clone()
        } else {
            match &known.directory {
                Some(directory) if directory.is_absolute() => directory.join(&known.path),
                // No build cwd is recorded for a relative legacy IR filename.
                _ => return known.path == source && source.is_relative(),
            }
        };
        // Resolve existing paths with OS semantics: collapsing symlink/.. lexically
        // can select a different source. Unresolved legacy paths match literally.
        match (path.canonicalize(), requested.canonicalize()) {
            (Ok(left), Ok(right)) => left == right,
            _ => path == requested,
        }
    })
}

/// Select entries using only known associations. This does not inspect or merge files.
pub fn select_modules(
    catalog: &ModuleCatalog,
    selection: &ModuleSelection,
    cwd: &Path,
) -> Result<ModuleCatalog, Error> {
    catalog.validate()?;
    for id in &selection.module_ids {
        if !catalog.modules.iter().any(|module| &module.id == id) {
            return Err(Error::InvalidArguments(format!(
                "unmatched module selector: {id}"
            )));
        }
    }
    for id in &selection.configuration_ids {
        if !catalog
            .modules
            .iter()
            .any(|module| module.configuration_id.as_ref() == Some(id))
        {
            return Err(Error::InvalidArguments(format!(
                "unmatched known configuration: {id}"
            )));
        }
    }
    for source in &selection.sources {
        if !catalog
            .modules
            .iter()
            .any(|module| source_matches(module, source, cwd))
        {
            return Err(Error::InvalidArguments(format!(
                "unmatched known source: {}",
                source.display()
            )));
        }
    }
    let mut selected = catalog.clone();
    selected.modules.retain(|module| {
        (selection.module_ids.is_empty() || selection.module_ids.contains(&module.id))
            && (selection.configuration_ids.is_empty()
                || module
                    .configuration_id
                    .as_ref()
                    .is_some_and(|id| selection.configuration_ids.contains(id)))
            && (selection.sources.is_empty()
                || selection
                    .sources
                    .iter()
                    .any(|source| source_matches(module, source, cwd)))
    });
    if selected.modules.is_empty()
        && !(selection.module_ids.is_empty()
            && selection.sources.is_empty()
            && selection.configuration_ids.is_empty())
    {
        return Err(Error::InvalidArguments(
            "selectors have an empty intersection".into(),
        ));
    }
    let has_filters = |s: &ModuleSelection| {
        !s.module_ids.is_empty() || !s.sources.is_empty() || !s.configuration_ids.is_empty()
    };
    if has_filters(selection) {
        if has_filters(&catalog.scope.selection) {
            selected
                .scope
                .selection_history
                .push(catalog.scope.selection.clone());
        }
        selected.scope.selection = selection.clone();
    }
    selected.scope.selected_entries = selected.modules.len();
    selected.scope.whole_program_complete = None;
    Ok(selected)
}

/// Copy a selected module to a new file, rechecking its recorded content hash.
fn copy_module(
    module: &ModuleRecord,
    output: &Path,
    archives: &mut super::inventory::ArchiveCache,
) -> Result<(), Error> {
    let source = module
        .path
        .as_ref()
        .ok_or_else(|| Error::MissingFile(format!("module {} has no path", module.id)))?;
    if !source.is_absolute() {
        return Err(Error::InvalidArguments(
            "resolve catalog paths through inventory before copying".into(),
        ));
    }
    if !fs::metadata(source)?.is_file() {
        return Err(Error::InvalidArguments(
            "module path is not a regular file".into(),
        ));
    }
    let parent = output.parent().unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(member) = &module.archive_member {
        temporary.write_all(archives.module(source, member)?)?;
    } else {
        std::io::copy(&mut fs::File::open(source)?, &mut temporary)?;
    }
    let actual = hash_file(temporary.path())?;
    if module
        .content_sha256
        .as_ref()
        .is_none_or(|expected| !actual.eq_ignore_ascii_case(expected))
    {
        return Err(Error::InvalidArguments(format!(
            "module content hash mismatch: {}",
            module.id
        )));
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

/// Publish selected modules and a relative-path catalog in a new directory.
/// Input paths must have been resolved by [`super::inventory`].
pub fn copy_modules(catalog: &ModuleCatalog, output_dir: &Path) -> Result<ModuleCatalog, Error> {
    catalog.validate()?;
    if catalog.modules.is_empty() {
        return Err(Error::InvalidArguments("no modules selected".into()));
    }
    if let Some(module) = catalog
        .modules
        .iter()
        .find(|module| module.status != ModuleStatus::Available)
    {
        return Err(Error::MissingFile(format!(
            "selected module {} is {:?}: {}",
            module.id,
            module.status,
            module.diagnostics.join("; ")
        )));
    }
    fs::create_dir(output_dir)?;
    let mut copied = catalog.clone();
    let mut archives = super::inventory::ArchiveCache::default();
    let mut groups = BTreeMap::new();
    for (index, module) in copied.modules.iter_mut().enumerate() {
        let parent = module
            .path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let next_group = groups.len();
        let group = groups.entry(parent).or_insert(next_group);
        let group_dir = output_dir.join(format!("{group:06}"));
        fs::create_dir_all(&group_dir)?;
        let filename = format!(
            "{group:06}/{index:06}-{}.bc",
            &hash_bytes(module.id.as_bytes())[..16]
        );
        copy_module(module, &output_dir.join(&filename), &mut archives)?;
        module.path = Some(filename.into());
        module.archive_member = None;
        if let Some(path) = &module.diagnostic_path {
            if path.is_file() {
                let name = format!("{index:06}.stderr.log");
                let mut temporary = tempfile::NamedTempFile::new_in(output_dir)?;
                std::io::copy(&mut fs::File::open(path)?, &mut temporary)?;
                temporary
                    .persist_noclobber(output_dir.join(&name))
                    .map_err(|error| Error::Io(error.error))?;
                module.diagnostic_path = Some(name.into());
            } else {
                module
                    .diagnostics
                    .push("original diagnostic file is unavailable".into());
                module.diagnostic_path = None;
            }
        }
    }
    copied.scope.whole_program_complete = None;
    write_catalog(&output_dir.join("catalog.json"), &copied)?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogOrigin, ModuleRecord, SourceAssociation, hash_file, read_catalog};
    use std::{fs, path::Path};

    fn fixture(root: &Path) -> ModuleCatalog {
        let modules = ["debug", "release"]
            .into_iter()
            .map(|configuration| {
                let path = root.join(format!("{configuration}.bc"));
                fs::write(&path, b"BC\xc0\xdesame contents").unwrap();
                let mut module = ModuleRecord::new(configuration);
                module.content_sha256 = Some(hash_file(&path).unwrap());
                module.path = Some(path);
                module.configuration_id = Some(configuration.into());
                module.status = ModuleStatus::Available;
                module.sources.push(SourceAssociation {
                    path: "src/source.c".into(),
                    directory: Some(root.into()),
                    origin: "compilation_database".into(),
                    content_sha256: None,
                });
                module
            })
            .collect();
        ModuleCatalog::new(
            CatalogOrigin {
                kind: "test".into(),
                input: "app".into(),
                sha256: None,
            },
            "recorded_modules",
            modules,
        )
    }

    #[test]
    fn source_and_configuration_filters_preserve_distinct_compilations() {
        let root = tempfile::tempdir().unwrap();
        let catalog = fixture(root.path());
        let by_source = ModuleSelection {
            sources: vec!["src/source.c".into()],
            ..Default::default()
        };
        assert_eq!(
            select_modules(&catalog, &by_source, root.path())
                .unwrap()
                .modules
                .len(),
            2
        );
        let selection = ModuleSelection {
            configuration_ids: vec!["debug".into()],
            ..by_source
        };
        let selected = select_modules(&catalog, &selection, root.path()).unwrap();
        assert_eq!(selected.modules[0].id, "debug");
        assert_eq!(selected.scope.total_entries, 2);
        assert_eq!(selected.scope.selected_entries, 1);
        let wrong = ModuleSelection {
            module_ids: vec!["missing".into()],
            ..Default::default()
        };
        assert!(select_modules(&catalog, &wrong, root.path()).is_err());
        let disjoint = ModuleSelection {
            module_ids: vec!["release".into()],
            ..selection
        };
        assert!(select_modules(&catalog, &disjoint, root.path()).is_err());
    }

    #[test]
    fn unknown_configuration_is_not_inferred_from_identical_contents() {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = fixture(root.path());
        for module in &mut catalog.modules {
            module.configuration_id = None;
        }
        let selection = ModuleSelection {
            configuration_ids: vec!["debug".into()],
            ..Default::default()
        };
        assert!(select_modules(&catalog, &selection, root.path()).is_err());
    }

    #[test]
    fn a_relative_source_selector_respects_known_compilation_directories() {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = fixture(root.path());
        catalog.modules[1].sources[0].directory = Some(root.path().join("other"));
        let selection = ModuleSelection {
            sources: vec!["src/source.c".into()],
            ..Default::default()
        };
        let selected = select_modules(&catalog, &selection, root.path()).unwrap();
        assert_eq!(selected.modules.len(), 1);
        assert_eq!(selected.modules[0].id, "debug");
    }

    #[test]
    fn reusing_a_catalog_preserves_its_recorded_selection() {
        let root = tempfile::tempdir().unwrap();
        let catalog = fixture(root.path());
        let selection = ModuleSelection {
            sources: vec!["src/source.c".into()],
            ..Default::default()
        };
        let selected = select_modules(&catalog, &selection, root.path()).unwrap();
        let reused = select_modules(&selected, &ModuleSelection::default(), root.path()).unwrap();
        assert_eq!(reused.scope.selection.sources, selection.sources);
        let narrowed = select_modules(
            &selected,
            &ModuleSelection {
                module_ids: vec!["debug".into()],
                ..Default::default()
            },
            root.path(),
        )
        .unwrap();
        assert_eq!(
            narrowed.scope.selection_history[0].sources,
            selection.sources
        );
    }

    #[test]
    fn copies_only_selected_modules_and_keeps_ids_after_relocation() {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = fixture(root.path());
        catalog.modules[1].status = ModuleStatus::Missing;
        fs::remove_file(catalog.modules[1].path.as_ref().unwrap()).unwrap();
        let selection = ModuleSelection {
            module_ids: vec!["debug".into()],
            ..Default::default()
        };
        let selected = select_modules(&catalog, &selection, root.path()).unwrap();
        let output = root.path().join("selected");
        let copied = copy_modules(&selected, &output).unwrap();
        assert_eq!(copied.modules[0].id, "debug");
        assert!(copied.modules[0].path.as_ref().unwrap().is_relative());
        let relocated = root.path().join("relocated");
        fs::rename(&output, &relocated).unwrap();
        let read = read_catalog(&relocated.join("catalog.json")).unwrap();
        let path = relocated.join(read.modules[0].path.as_ref().unwrap());
        assert_eq!(
            hash_file(&path).unwrap(),
            read.modules[0].content_sha256.as_ref().unwrap().as_str()
        );
        assert_eq!(read.scope.selected_entries, 1);
        assert_eq!(read.scope.total_entries, 2);
        assert!(copy_modules(&selected, &relocated).is_err());
        assert!(path.exists());
    }

    #[test]
    fn failed_selection_and_hash_mismatch_do_not_publish_a_catalog() {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = fixture(root.path());
        catalog.modules[1].status = ModuleStatus::Missing;
        let output = root.path().join("missing");
        assert!(copy_modules(&catalog, &output).is_err());
        assert!(!output.exists());
        catalog.modules.pop();
        catalog.scope.selected_entries = 1;
        fs::write(catalog.modules[0].path.as_ref().unwrap(), b"changed").unwrap();
        let output = root.path().join("changed");
        assert!(copy_modules(&catalog, &output).is_err());
        assert!(!output.join("catalog.json").exists());
    }
}
