//! Read existing module evidence without compiling or merging.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use super::{
    ArchiveMember, CatalogOrigin, ModuleCatalog, ModuleRecord, ModuleStatus, SourceAssociation,
    hash_bytes, hash_file, identity, read_catalog,
};
use crate::{
    bitcode_info::find_llvm_dis, error::Error, utils::extract_bitcode_filepaths_from_parsed_object,
};

fn is_bitcode(data: &[u8]) -> bool {
    data.starts_with(b"BC\xc0\xde") || data.starts_with(&[0xde, 0xc0, 0x17, 0x0b])
}

/// Inspect one module with an explicitly selected reader. Failures stay in the record.
pub fn inspect_bitcode(path: &Path, llvm_dis: &Path, id: impl Into<String>) -> ModuleRecord {
    let mut module = ModuleRecord::new(id);
    module.path = Some(path.to_path_buf());
    module.unavailable_metadata = [
        "compiler",
        "configuration_id",
        "build_identity",
        "source_snapshot",
        "ir_stage",
    ]
    .map(String::from)
    .to_vec();
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            module.status = ModuleStatus::Unsupported;
            module
                .diagnostics
                .push("module path is not a regular file".into());
            return module;
        }
        Err(error) => {
            module.status = if error.kind() == std::io::ErrorKind::NotFound {
                ModuleStatus::Missing
            } else {
                ModuleStatus::Failed
            };
            module.diagnostics.push(error.to_string());
            return module;
        }
    }
    let result = (|| -> Result<(), Error> {
        let hash = hash_file(path)?;
        module.content_sha256 = Some(hash.clone());
        let output = Command::new(llvm_dis)
            .arg(path)
            .args(["-o", "-"])
            .output()?;
        if !output.status.success() {
            return Err(Error::ExecutionFailure(format!(
                "llvm-dis exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let ir = String::from_utf8(output.stdout)?;
        if hash_file(path)? != hash {
            return Err(Error::ExecutionFailure(
                "module changed during inspection".into(),
            ));
        }
        parse_metadata(&ir, &mut module);
        module.content_sha256 = Some(hash);
        Ok(())
    })();
    match result {
        Ok(()) => module.status = ModuleStatus::Available,
        Err(error) => {
            module.status = ModuleStatus::Failed;
            module.diagnostics.push(error.to_string());
        }
    }
    module
}

// LLVM IR uses byte escapes (\XX), not shell or Rust string escaping.
fn quoted(text: &str) -> Option<String> {
    let mut bytes = text.trim_start().strip_prefix('"')?.bytes();
    let mut value = Vec::new();
    while let Some(byte) = bytes.next() {
        match byte {
            b'"' => return String::from_utf8(value).ok(),
            b'\\' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                value.push((high * 16 + low) as u8);
            }
            _ => value.push(byte),
        }
    }
    None
}

fn parse_metadata(ir: &str, module: &mut ModuleRecord) {
    let mut sources = BTreeSet::new();
    for line in ir.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("target triple = ") {
            module.target_triple = quoted(value);
        }
        if let Some(value) = line.strip_prefix("target datalayout = ") {
            module.data_layout = quoted(value);
        }
        if let Some(value) = line.strip_prefix("source_filename = ")
            && let Some(path) = quoted(value)
            && !path.is_empty()
        {
            sources.insert((path.clone(), None));
            module.sources.push(SourceAssociation {
                path: path.into(),
                directory: None,
                origin: "ir_source_filename".into(),
                content_sha256: None,
            });
        }
        if line.contains("!DIFile(")
            && let Some((_, value)) = line.split_once("filename:")
            && let Some(path) = quoted(value)
        {
            let directory = line
                .split_once("directory:")
                .and_then(|(_, s)| quoted(s))
                .filter(|s| !s.is_empty());
            if sources.insert((path.clone(), directory.clone())) {
                module.sources.push(SourceAssociation {
                    path: path.into(),
                    directory: directory.map(PathBuf::from),
                    origin: "debug_info".into(),
                    content_sha256: None,
                });
            }
        }
    }
    module.debug_info = Some(ir.contains("!llvm.dbg.cu") || ir.contains("!DICompileUnit("));
    if module.target_triple.is_none() {
        module.unavailable_metadata.push("target_triple".into());
    }
    if module.data_layout.is_none() {
        module.unavailable_metadata.push("data_layout".into());
    }
    if module.sources.is_empty() {
        module.unavailable_metadata.push("sources".into());
    }
}

struct MemberRange {
    name: String,
    offset: u64,
    size: u64,
}
struct ArchiveData {
    bytes: Vec<u8>,
    members: Vec<Result<MemberRange, String>>,
}

impl ArchiveData {
    fn read(path: &Path) -> Result<Self, Error> {
        if !fs::metadata(path)?.is_file() {
            return Err(Error::InvalidArguments(
                "archive must be a regular file".into(),
            ));
        }
        let bytes = fs::read(path)?;
        let archive = object::read::archive::ArchiveFile::parse(&*bytes)?;
        if archive.is_thin() {
            return Err(Error::UnsupportedBinaryFormat(
                "thin archives are not supported; create a regular archive".into(),
            ));
        }
        let mut members = Vec::new();
        for member in archive.members() {
            match member {
                Ok(member) => {
                    let (offset, size) = member.file_range();
                    members.push(Ok(MemberRange {
                        name: String::from_utf8_lossy(member.name()).into_owned(),
                        offset,
                        size,
                    }));
                }
                Err(error) => {
                    members.push(Err(error.to_string()));
                    break;
                }
            }
        }
        Ok(Self { bytes, members })
    }
}

/// Reuse one archive buffer and one member index per inventory/copy operation.
#[derive(Default)]
pub(crate) struct ArchiveCache {
    archives: BTreeMap<PathBuf, ArchiveData>,
}

impl ArchiveCache {
    pub(crate) fn module(&mut self, path: &Path, member: &ArchiveMember) -> Result<&[u8], Error> {
        let archive = match self.archives.entry(path.to_path_buf()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(ArchiveData::read(path)?),
        };
        let actual = archive
            .members
            .get(member.index)
            .ok_or_else(|| {
                Error::MissingFile(format!("archive member {} is missing", member.name))
            })?
            .as_ref()
            .map_err(|error| Error::InvalidArguments(error.clone()))?;
        if actual.name != member.name {
            return Err(Error::InvalidArguments(
                "archive member identity changed".into(),
            ));
        }
        let range = usize::try_from(actual.offset)
            .ok()
            .zip(usize::try_from(actual.size).ok())
            .and_then(|(start, size)| start.checked_add(size).map(|end| start..end));
        range
            .and_then(|range| archive.bytes.get(range))
            .ok_or_else(|| Error::InvalidArguments("archive member data is out of bounds".into()))
    }
}

fn inspect_archive_member(
    path: &Path,
    member: &ArchiveMember,
    tool: &Path,
    id: &str,
    bytes: Result<&[u8], Error>,
) -> ModuleRecord {
    let result = (|| -> Result<ModuleRecord, Error> {
        let mut temporary = tempfile::NamedTempFile::new()?;
        temporary.write_all(bytes?)?;
        Ok(inspect_bitcode(temporary.path(), tool, id))
    })();
    let mut module = result.unwrap_or_else(|error| {
        let mut module = ModuleRecord::new(id);
        module.status = ModuleStatus::Failed;
        module.diagnostics.push(error.to_string());
        module
    });
    module.path = Some(path.to_path_buf());
    module.archive_member = Some(member.clone());
    module
}

/// Inventory an artifact or catalog. This never invokes a compiler or merger.
pub fn inventory(
    input: &Path,
    bitcode_root: &Path,
    llvm_dis: Option<&Path>,
) -> Result<ModuleCatalog, Error> {
    let input = input.canonicalize()?;
    if !fs::metadata(&input)?.is_file() {
        return Err(Error::InvalidArguments(
            "inventory input must be a regular file".into(),
        ));
    }
    let data = fs::read(&input)?;
    let tool = match llvm_dis {
        Some(path) => path.to_path_buf(),
        None => find_llvm_dis()?,
    };
    if data.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') {
        let mut catalog = read_catalog(&input)?;
        let root = input.parent().expect("canonical file has a parent");
        let mut archives = ArchiveCache::default();
        for module in &mut catalog.modules {
            if let Some(path) = &module.diagnostic_path
                && path.is_relative()
            {
                module.diagnostic_path = Some(root.join(path));
            }
            let Some(path) = &module.path else {
                continue;
            };
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            module.path = Some(path.clone());
            if !matches!(
                module.status,
                ModuleStatus::Available | ModuleStatus::Missing
            ) {
                continue;
            }
            let observed = if let Some(member) = &module.archive_member {
                inspect_archive_member(
                    &path,
                    member,
                    &tool,
                    &module.id,
                    archives.module(&path, member),
                )
            } else {
                inspect_bitcode(&path, &tool, &module.id)
            };
            if module
                .content_sha256
                .as_ref()
                .zip(observed.content_sha256.as_ref())
                .is_some_and(|(expected, actual)| !expected.eq_ignore_ascii_case(actual))
            {
                module.status = ModuleStatus::Failed;
                module
                    .diagnostics
                    .push("module content hash mismatch".into());
                continue;
            }
            if observed.status == ModuleStatus::Available {
                module.status = ModuleStatus::Available;
                module.content_sha256 = observed.content_sha256;
                module.target_triple = observed.target_triple;
                module.data_layout = observed.data_layout;
                module.debug_info = observed.debug_info;
                if module.sources.is_empty() {
                    module.sources = observed.sources;
                }
            } else {
                module.status = observed.status;
                module.diagnostics.extend(observed.diagnostics);
            }
        }
        catalog.scope.whole_program_complete = None;
        return Ok(catalog);
    }

    let origin = CatalogOrigin {
        kind: "artifact".into(),
        input: input.clone(),
        sha256: Some(hash_bytes(&data)),
    };
    let mut modules = Vec::new();
    let mut references = BTreeSet::new();
    let mut boundaries = Vec::new();
    if is_bitcode(&data) {
        modules.push(inspect_bitcode(
            &input,
            &tool,
            identity(&["bitcode", &hash_bytes(&data)]),
        ));
    } else if let Ok(object) = object::File::parse(&*data) {
        references.extend(extract_bitcode_filepaths_from_parsed_object(&object)?);
        if references.is_empty() {
            boundaries.push("input object contains no recorded module references".into());
        }
    } else if let Ok(archive) = object::read::archive::ArchiveFile::parse(&*data) {
        if archive.is_thin() {
            return Err(Error::UnsupportedBinaryFormat(
                "thin archives are not supported; create a regular archive".into(),
            ));
        }
        for (index, member) in archive.members().enumerate() {
            let member = match member {
                Ok(member) => member,
                Err(error) => {
                    let mut failed = ModuleRecord::new(identity(&[
                        "unreadable_archive_entry",
                        origin.sha256.as_deref().unwrap_or(""),
                        &index.to_string(),
                    ]));
                    failed.status = ModuleStatus::Failed;
                    failed
                        .diagnostics
                        .push(format!("archive traversal failed: {error}"));
                    failed.unavailable_metadata.push("archive_member".into());
                    boundaries.push(format!(
                        "archive traversal stopped at entry {index}: {error}"
                    ));
                    modules.push(failed);
                    break;
                }
            };
            let name = String::from_utf8_lossy(member.name()).into_owned();
            let bytes = match member.data(&*data) {
                Ok(bytes) => bytes,
                Err(error) => {
                    boundaries.push(format!("unreadable archive member {name}: {error}"));
                    let mut failed = ModuleRecord::new(identity(&[
                        "unreadable_archive_member",
                        origin.sha256.as_deref().unwrap_or(""),
                        &index.to_string(),
                        &name,
                    ]));
                    failed.status = ModuleStatus::Failed;
                    failed.path = Some(input.clone());
                    failed.archive_member = Some(ArchiveMember {
                        index,
                        name: name.clone(),
                    });
                    failed.diagnostics.push(error.to_string());
                    failed.unavailable_metadata.push("module_kind".into());
                    modules.push(failed);
                    continue;
                }
            };
            if is_bitcode(bytes) {
                let reference = ArchiveMember {
                    index,
                    name: name.clone(),
                };
                let id = identity(&[
                    "archive_member",
                    origin.sha256.as_deref().unwrap_or(""),
                    &index.to_string(),
                    &name,
                ]);
                modules.push(inspect_archive_member(
                    &input,
                    &reference,
                    &tool,
                    &id,
                    Ok(bytes),
                ));
            } else if let Ok(object) = object::File::parse(bytes) {
                let paths = extract_bitcode_filepaths_from_parsed_object(&object)?;
                if paths.is_empty() {
                    boundaries.push(format!(
                        "archive object {name} contains no recorded module references"
                    ));
                }
                references.extend(paths);
            } else {
                boundaries.push(format!(
                    "archive member {name} has no supported module evidence"
                ));
            }
        }
    } else {
        return Err(Error::UnsupportedBinaryFormat(
            "expected bitcode, object, archive, or module catalog".into(),
        ));
    }
    for recorded in references {
        let path = if recorded.is_absolute() {
            recorded.clone()
        } else {
            bitcode_root.join(&recorded)
        };
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut module = inspect_bitcode(
            &path,
            &tool,
            identity(&["recorded_module", &recorded.to_string_lossy()]),
        );
        module.recorded_path = Some(recorded);
        modules.push(module);
    }
    let mut catalog = ModuleCatalog::new(origin, "recorded_modules", modules);
    catalog.scope.limitations.extend(boundaries);
    catalog.scope.limitations.push("Legacy path sections do not record compiler settings, build/source snapshots, or IR stage.".into());
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ModuleStatus, hash_file, write_catalog};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, path::Path};

    fn reader(root: &Path, exit: i32) -> std::path::PathBuf {
        let path = root.join(format!("llvm-dis-{exit}"));
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' 'source_filename = \"src/main.c\"' 'target triple = \"x86_64-test\"' 'target datalayout = \"e-p:64:64\"'\nexit {exit}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn archive_bytes() -> Vec<u8> {
        let mut bytes = b"!<arch>\n".to_vec();
        for (name, data) in [
            ("first.bc/", b"BC\xc0\xdefirst".as_slice()),
            ("second.bc/", b"BC\xc0\xdesecond".as_slice()),
        ] {
            bytes.extend(
                format!(
                    "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                    0,
                    0,
                    0,
                    "100644",
                    data.len()
                )
                .bytes(),
            );
            bytes.extend_from_slice(data);
            if data.len() % 2 != 0 {
                bytes.push(b'\n');
            }
        }
        bytes
    }

    #[test]
    fn a_corrupt_archive_tail_retains_earlier_module_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("partial.bca");
        let mut data = archive_bytes();
        data.extend_from_slice(b"broken member header");
        fs::write(&path, data).unwrap();
        let catalog = inventory(&path, root.path(), Some(&reader(root.path(), 0))).unwrap();
        assert_eq!(
            catalog
                .modules
                .iter()
                .filter(|m| m.status == ModuleStatus::Available)
                .count(),
            2
        );
        assert!(
            catalog
                .modules
                .iter()
                .any(|m| m.status == ModuleStatus::Failed)
        );
        assert!(
            catalog
                .scope
                .limitations
                .iter()
                .any(|s| s.contains("archive"))
        );
    }

    #[test]
    fn thin_archives_are_explicitly_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("thin.a");
        fs::write(&path, b"!<thin>\n").unwrap();
        let error = inventory(&path, root.path(), Some(&reader(root.path(), 0))).unwrap_err();
        assert!(error.to_string().contains("thin"));
    }

    #[test]
    fn archive_members_share_one_snapshot_within_an_operation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("modules.bca");
        fs::write(&path, archive_bytes()).unwrap();
        let mut cache = ArchiveCache::default();
        assert_eq!(
            cache
                .module(
                    &path,
                    &ArchiveMember {
                        index: 0,
                        name: "first.bc".into()
                    }
                )
                .unwrap(),
            b"BC\xc0\xdefirst"
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            cache
                .module(
                    &path,
                    &ArchiveMember {
                        index: 1,
                        name: "second.bc".into()
                    }
                )
                .unwrap(),
            b"BC\xc0\xdesecond"
        );
    }

    #[test]
    fn identifiable_unreadable_members_remain_failed_entries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("partial.bca");
        let mut data = archive_bytes();
        data.extend(
            format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                "missing.bc/", 0, 0, 0, "100644", 10000
            )
            .bytes(),
        );
        fs::write(&path, data).unwrap();
        let catalog = inventory(&path, root.path(), Some(&reader(root.path(), 0))).unwrap();
        assert_eq!(
            catalog
                .modules
                .iter()
                .filter(|m| m.status == ModuleStatus::Available)
                .count(),
            2
        );
        assert!(catalog.modules.iter().any(|m| {
            m.status == ModuleStatus::Failed
                && m.archive_member
                    .as_ref()
                    .is_some_and(|member| member.name == "missing.bc")
        }));
    }

    #[test]
    fn metadata_decodes_source_paths_and_keeps_debug_origins() {
        let ir = "source_filename = \"src/a\\22b.c\"\ntarget triple = \"aarch64-test\"\ntarget datalayout = \"e-p:64:64\"\n!0 = !DIFile(filename: \"header.h\", directory: \"/build/include\")\n!llvm.dbg.cu = !{!1}\n";
        let mut module = crate::catalog::ModuleRecord::new("module");
        parse_metadata(ir, &mut module);
        assert_eq!(module.sources[0].path, Path::new("src/a\"b.c"));
        assert_eq!(
            module.sources[1].directory.as_deref(),
            Some(Path::new("/build/include"))
        );
        assert_eq!(module.sources[1].origin, "debug_info");
        assert_eq!(module.target_triple.as_deref(), Some("aarch64-test"));
        assert_eq!(module.debug_info, Some(true));
        assert!(module.compiler.is_none());
        assert!(module.source_snapshot.is_none());
    }

    #[test]
    fn failed_reader_never_turns_stdout_into_available_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.bc");
        fs::write(&path, b"BC\xc0\xdepayload").unwrap();
        let module = inspect_bitcode(&path, &reader(dir.path(), 7), "module");
        assert_eq!(module.status, ModuleStatus::Failed);
        assert!(module.target_triple.is_none());
        assert!(!module.diagnostics.is_empty());
    }

    #[test]
    fn inventory_keeps_missing_references_and_does_not_merge() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("present.bc"), b"BC\xc0\xdepayload").unwrap();
        let mut object = object::write::Object::new(
            object::BinaryFormat::Elf,
            object::Architecture::X86_64,
            object::Endianness::Little,
        );
        let section = object.add_section(
            vec![],
            b".rllvm_bc".to_vec(),
            object::SectionKind::OtherString,
        );
        object.append_section_data(section, b"present.bc\nmissing.bc\n", 1);
        let input = dir.path().join("input.o");
        fs::write(&input, object.write().unwrap()).unwrap();
        let catalog = inventory(&input, dir.path(), Some(&reader(dir.path(), 0))).unwrap();
        assert_eq!(catalog.modules.len(), 2);
        assert_eq!(
            catalog
                .modules
                .iter()
                .filter(|m| m.status == ModuleStatus::Available)
                .count(),
            1
        );
        assert_eq!(
            catalog
                .modules
                .iter()
                .filter(|m| m.status == ModuleStatus::Missing)
                .count(),
            1
        );
        assert!(catalog.scope.whole_program_complete.is_none());
        assert!(catalog.modules.iter().all(|m| m.configuration_id.is_none()));
    }

    #[test]
    fn catalog_paths_resolve_from_the_catalog_and_hash_mismatches_are_retained() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.bc");
        fs::write(&path, b"BC\xc0\xdepayload").unwrap();
        let tool = reader(dir.path(), 0);
        let mut catalog = inventory(&path, dir.path(), Some(&tool)).unwrap();
        catalog.modules[0].path = Some("module.bc".into());
        let catalog_path = dir.path().join("catalog.json");
        write_catalog(&catalog_path, &catalog).unwrap();
        let loaded = inventory(&catalog_path, Path::new("/unrelated"), Some(&tool)).unwrap();
        assert_eq!(
            loaded.modules[0].path.as_deref(),
            Some(path.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            loaded.modules[0].content_sha256.as_deref(),
            Some(hash_file(&path).unwrap().as_str())
        );
        fs::write(&path, b"BC\xc0\xdechanged").unwrap();
        let loaded = inventory(&catalog_path, dir.path(), Some(&tool)).unwrap();
        assert_eq!(loaded.modules[0].status, ModuleStatus::Failed);
        assert!(
            loaded.modules[0]
                .diagnostics
                .iter()
                .any(|s| s.contains("hash mismatch"))
        );
    }
}
