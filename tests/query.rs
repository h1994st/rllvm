use std::{
    path::{Path, PathBuf},
    process::Command,
};

use rllvm::catalog::read_catalog;
use rllvm::query::load::{for_each_module, load_catalog};

#[test]
fn query_binary_reports_its_llvm_major() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .arg("--llvm-version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("23."), "unexpected LLVM version: {text}");
}

fn llvm_bin(name: &str) -> PathBuf {
    let config = rllvm::utils::find_llvm_config().unwrap();
    let output = Command::new(config).arg("--bindir").output().unwrap();
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).unwrap().trim()).join(name)
}

#[test]
fn a_module_whose_bytes_changed_is_excluded_without_shrinking_scope() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, module_path) = write_catalog_with_one_module(&scratch);

    // Corrupt the module after the catalog recorded its hash.
    std::fs::write(&module_path, b"not bitcode").unwrap();

    let loaded = load_catalog(&catalog_path).unwrap();
    assert!(
        loaded.pending.is_empty(),
        "changed module must not be parsed"
    );
    assert_eq!(
        loaded.reports[0].status,
        rllvm::query::ModuleAnalysis::Changed
    );
    assert_eq!(
        loaded.scope.selected_entries,
        read_catalog(&catalog_path).unwrap().scope.selected_entries,
        "scope must not shrink when a module fails verification"
    );
}

#[test]
fn an_edited_source_marks_its_locations_modified() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, _) = write_catalog_with_one_module(&scratch);
    let source = scratch.path().join("add.c");

    // `inventory()` records source associations with `content_sha256: None`
    // (`catalog/inventory.rs:123`), so staleness is undecidable from its
    // output alone. Stamp the hash the capture would have had, then edit.
    record_source_hash(&catalog_path, &source);
    std::fs::write(&source, "int add(int a,int b){return a+b;} /* edited */\n").unwrap();

    let loaded = load_catalog(&catalog_path).unwrap();
    assert_eq!(
        loaded
            .source_status
            .get(&("add".to_string(), source))
            .copied(),
        Some(rllvm::query::SourceStatus::Modified)
    );
}

#[test]
fn a_source_without_a_recorded_hash_is_unknown_not_modified() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, _) = write_catalog_with_one_module(&scratch);
    let source = scratch.path().join("add.c");
    std::fs::write(&source, "int add(int a,int b){return a+b;} /* edited */\n").unwrap();

    // No hash was recorded, so an edit cannot be detected and must not be
    // claimed. This is the behaviour `inventory()` actually produces today.
    let loaded = load_catalog(&catalog_path).unwrap();
    assert_eq!(
        loaded
            .source_status
            .get(&("add".to_string(), source))
            .copied(),
        Some(rllvm::query::SourceStatus::Unknown)
    );
}

#[test]
fn two_modules_recording_one_source_keep_separate_statuses() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog_path = two_modules_one_source(&scratch); // "fresh" and "stale"
    let source = scratch.path().join("shared.c");

    let loaded = load_catalog(&catalog_path).unwrap();
    assert_eq!(
        loaded
            .source_status
            .get(&("fresh".to_string(), source.clone()))
            .copied(),
        Some(rllvm::query::SourceStatus::Current)
    );
    assert_eq!(
        loaded
            .source_status
            .get(&("stale".to_string(), source))
            .copied(),
        Some(rllvm::query::SourceStatus::Modified),
        "a path-only key would let one module overwrite the other"
    );
}

#[test]
fn an_archive_member_loads_through_the_existing_cache() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog_path = archive_catalog(&scratch); // two .bc members in one .a

    let loaded = load_catalog(&catalog_path).unwrap();
    assert_eq!(loaded.pending.len(), 2, "both archive members must load");

    // `PendingModule` has no `bytes` field: reading bytes is `for_each_module`'s
    // job. Verify here that both members resolved to the expected archive
    // path with distinct member indices.
    let archive_path = scratch.path().join("lib.a").canonicalize().unwrap();
    let mut indices: Vec<usize> = loaded
        .pending
        .iter()
        .map(|module| {
            assert_eq!(
                module.path, archive_path,
                "each member must resolve to the archive path"
            );
            module
                .member
                .as_ref()
                .expect("an archive module must carry a member reference")
                .index
        })
        .collect();
    indices.sort_unstable();
    assert_eq!(
        indices,
        vec![0, 1],
        "members must resolve to distinct indices"
    );

    // Drive the actual byte reads through `for_each_module` to prove the
    // cached archive bytes really load.
    let mut loaded_count = 0;
    for_each_module(&loaded, |module| {
        assert!(!module.bytes.is_empty(), "archive member bytes must load");
        loaded_count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(loaded_count, 2);
}

/// Builds a real one-module catalog by compiling a source with the configured
/// clang and running the inventory, so the recorded hashes are genuine.
///
/// The module id is fixed to "add" so callers can key `source_status`
/// lookups on it: `inventory()` derives ids from a content hash, which is not
/// otherwise predictable from the fixture.
fn write_catalog_with_one_module(scratch: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let source = scratch.path().join("add.c");
    std::fs::write(&source, "int add(int a,int b){return a+b;}\n").unwrap();
    let module = scratch.path().join("add.bc");
    let status = std::process::Command::new(llvm_bin("clang"))
        .args(["-g", "-O0", "-emit-llvm", "-c"])
        .arg(&source)
        .arg("-o")
        .arg(&module)
        .status()
        .unwrap();
    assert!(status.success());

    let mut catalog =
        rllvm::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis"))).unwrap();
    catalog.modules[0].id = "add".to_string();
    let catalog_path = scratch.path().join("catalog.json");
    std::fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    (catalog_path, module)
}

/// Stamps the current source hash into the catalog's association, standing in
/// for a capture that recorded one.
fn record_source_hash(catalog_path: &Path, source: &Path) {
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(catalog_path).unwrap()).unwrap();
    let hash = rllvm::catalog::hash_bytes(&std::fs::read(source).unwrap());
    for module in catalog["modules"].as_array_mut().unwrap() {
        for association in module["sources"].as_array_mut().unwrap() {
            association["content_sha256"] = serde_json::Value::String(hash.clone());
        }
    }
    std::fs::write(catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
}

/// Builds a catalog with two modules, "fresh" and "stale", that both record a
/// source association with one shared source file. "fresh" is stamped with
/// the source's real current hash; "stale" is stamped with a hash that does
/// not match, standing in for a capture whose source has since changed.
fn two_modules_one_source(scratch: &tempfile::TempDir) -> PathBuf {
    let source = scratch.path().join("shared.c");
    std::fs::write(&source, "int shared(void){return 0;}\n").unwrap();
    let module = scratch.path().join("shared.bc");
    let status = std::process::Command::new(llvm_bin("clang"))
        .args(["-g", "-O0", "-emit-llvm", "-c"])
        .arg(&source)
        .arg("-o")
        .arg(&module)
        .status()
        .unwrap();
    assert!(status.success());

    let base =
        rllvm::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis"))).unwrap();
    let template = base.modules[0].clone();

    let current_hash = rllvm::catalog::hash_bytes(&std::fs::read(&source).unwrap());
    let stale_hash = rllvm::catalog::hash_bytes(b"stale content, does not match shared.c");

    let mut fresh = template.clone();
    fresh.id = "fresh".to_string();
    for association in &mut fresh.sources {
        association.content_sha256 = Some(current_hash.clone());
    }

    let mut stale = template;
    stale.id = "stale".to_string();
    for association in &mut stale.sources {
        association.content_sha256 = Some(stale_hash.clone());
    }

    let catalog =
        rllvm::catalog::ModuleCatalog::new(base.origin, "recorded_modules", vec![fresh, stale]);
    let path = scratch.path().join("two-module-catalog.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    path
}

/// Builds a real archive of two bitcode members and inventories it, so the
/// catalog carries genuine `archive_member` indices.
fn archive_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    let mut objects = Vec::new();
    for name in ["one", "two"] {
        let source = scratch.path().join(format!("{name}.c"));
        std::fs::write(&source, format!("int {name}(void){{return 0;}}\n")).unwrap();
        let object = scratch.path().join(format!("{name}.bc"));
        assert!(
            std::process::Command::new(llvm_bin("clang"))
                .args(["-g", "-O0", "-emit-llvm", "-c"])
                .arg(&source)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        objects.push(object);
    }
    let archive = scratch.path().join("lib.a");
    assert!(
        std::process::Command::new(llvm_bin("llvm-ar"))
            .arg("rs")
            .arg(&archive)
            .args(&objects)
            .status()
            .unwrap()
            .success()
    );
    let catalog =
        rllvm::catalog::inventory(&archive, scratch.path(), Some(&llvm_bin("llvm-dis"))).unwrap();
    let path = scratch.path().join("archive-catalog.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    path
}
