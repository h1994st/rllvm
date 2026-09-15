use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use rllvm::catalog::read_catalog;
use rllvm::query::CallTarget;
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

#[test]
fn the_binary_does_not_link_an_llvm_shared_library() {
    let binary = env!("CARGO_BIN_EXE_rllvm-query");
    let tool = if cfg!(target_os = "macos") {
        "otool"
    } else {
        "ldd"
    };
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["-L"]
    } else {
        &[]
    };
    let output = std::process::Command::new(tool)
        .args(args)
        .arg(binary)
        .output()
        .unwrap();
    // Without this the test passes when the inspection command itself fails:
    // an empty stdout trivially contains no "libLLVM".
    assert!(
        output.status.success(),
        "{tool} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !text.trim().is_empty(),
        "{tool} produced no output to inspect"
    );
    assert!(
        !text.contains("libLLVM"),
        "LLVM must be linked statically; found a dynamic reference:\n{text}"
    );
}

#[test]
fn the_cli_prints_callers_as_json() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch); // main calls add
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(&scratch))
        .arg("--catalog")
        .arg(&catalog)
        .args(["callers", "add"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["results"][0]["function"]["symbol"], "main");
}

fn llvm_bin(name: &str) -> PathBuf {
    let config = rllvm::utils::find_llvm_config().unwrap();
    let output = Command::new(config).arg("--bindir").output().unwrap();
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).unwrap().trim()).join(name)
}

/// Points `RLLVM_CONFIG` at a scratch file, matching `tests/integration.rs`'s
/// isolation convention: `rllvm-query` now reads the configured log level
/// (`rllvm_query.rs`, so a module that fails to extract is actually reported
/// on stderr), and without this the CLI test would depend on whatever the
/// developer happens to have at `~/.rllvm/config.toml`.
fn scratch_rllvm_config(scratch: &tempfile::TempDir) -> PathBuf {
    let contents = format!(
        "llvm_config_filepath = '{}'\n\
         clang_filepath = '{}'\n\
         clangxx_filepath = '{}'\n\
         llvm_ar_filepath = '{}'\n\
         llvm_link_filepath = '{}'\n",
        rllvm::utils::find_llvm_config().unwrap().display(),
        llvm_bin("clang").display(),
        llvm_bin("clang++").display(),
        llvm_bin("llvm-ar").display(),
        llvm_bin("llvm-link").display(),
    );
    let config_path = scratch.path().join("rllvm-config.toml");
    std::fs::write(&config_path, contents).unwrap();
    config_path
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

#[test]
fn a_deleted_archive_reports_its_members_missing_not_failed() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog_path = archive_catalog(&scratch);

    // The archive itself is gone, not just corrupted: `ArchiveData::read`
    // opens it via `fs::metadata`/`fs::read`, which fail with `NotFound`,
    // and that must surface as Missing rather than Failed.
    std::fs::remove_file(scratch.path().join("lib.a")).unwrap();

    let loaded = load_catalog(&catalog_path).unwrap();
    assert!(
        loaded.pending.is_empty(),
        "no module can load from a deleted archive"
    );
    assert_eq!(loaded.reports.len(), 2);
    assert!(
        loaded
            .reports
            .iter()
            .all(|report| report.status == rllvm::query::ModuleAnalysis::Missing),
        "{:?}",
        loaded.reports
    );
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

/// Builds a real two-module catalog: one module defines `add`, the other
/// defines `main`, which calls it. Combined with `llvm-ar rs` into one
/// archive and inventoried, following `archive_catalog`, so the catalog
/// carries genuine content hashes and `archive_member` indices, and exercises
/// the archive path the loader already supports.
fn two_module_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    let sources = [
        ("add.c", "int add(int a,int b){return a+b;}\n"),
        (
            "main.c",
            "int add(int a,int b);\nint main(void){ return add(2,3); }\n",
        ),
    ];
    let mut objects = Vec::new();
    for (name, source) in sources {
        let source_path = scratch.path().join(name);
        std::fs::write(&source_path, source).unwrap();
        let object = scratch.path().join(name.replace(".c", ".bc"));
        assert!(
            std::process::Command::new(llvm_bin("clang"))
                .args(["-g", "-O0", "-emit-llvm", "-c"])
                .arg(&source_path)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        objects.push(object);
    }
    let archive = scratch.path().join("two.a");
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
    let path = scratch.path().join("two-module-catalog.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    path
}

#[test]
fn a_module_that_vanishes_after_loading_is_reported_not_fatal() {
    // A concurrent build deletes or rewrites one `.bc` between
    // `load_catalog` and the read that actually wants its bytes. The
    // contract is the one `load_catalog` already follows: record the module
    // and carry on. Propagating the read error instead fails the whole run,
    // so the query answers nothing at all rather than answering with that
    // one module unaccounted for.
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, first_module) = two_plain_module_catalog(&scratch);

    let loaded = load_catalog(&catalog_path).unwrap();
    assert_eq!(loaded.pending.len(), 2, "both modules verify at load time");
    let vanished = loaded
        .pending
        .iter()
        .find(|module| module.path.file_name() == first_module.file_name())
        .expect("the catalog must name the module about to vanish")
        .id
        .clone();

    std::fs::remove_file(&first_module).unwrap();

    let mut visited: Vec<String> = Vec::new();
    let unreadable = for_each_module(&loaded, |module| {
        visited.push(module.id.clone());
        Ok(())
    })
    .expect("one unreadable module must not abort the walk");

    assert_eq!(
        visited.len(),
        1,
        "the surviving module must still be handed over: {visited:?}"
    );
    assert!(!visited.contains(&vanished));
    assert_eq!(unreadable.len(), 1);
    assert_eq!(unreadable[0].0, vanished);
}

#[test]
fn a_compdb_catalog_carries_source_status_into_an_answer() {
    // `inventory()` records `content_sha256: None` for every source
    // association, so a `rllvm-get-bc` catalog can only ever answer
    // `unknown`. `rllvm-compdb generate` hashes the source it compiles, and
    // this is the end-to-end proof that the hash survives the catalog, the
    // `(module, path)` key `build_location` looks up, and the envelope --
    // any of which silently degrades to `unknown` if the key shape drifts.
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("mod.c");
    std::fs::write(
        &source,
        "int helper(int x){return x+1;}\nint main(void){return helper(1);}\n",
    )
    .unwrap();
    std::fs::write(
        scratch.path().join("compile_commands.json"),
        serde_json::to_vec_pretty(&serde_json::json!([{
            "directory": scratch.path(),
            "file": source,
            "arguments": [llvm_bin("clang"), "-g", "-O0", "-c", source],
        }]))
        .unwrap(),
    )
    .unwrap();

    let generate = Command::new(env!("CARGO_BIN_EXE_rllvm-compdb"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(&scratch))
        .current_dir(scratch.path())
        .args(["generate", ".", "--output-dir", "analysis"])
        .output()
        .unwrap();
    assert!(
        generate.status.success(),
        "rllvm-compdb generate failed: {}",
        String::from_utf8_lossy(&generate.stderr)
    );
    let catalog = scratch.path().join("analysis/catalog.json");

    let current = query_json(&scratch, &catalog, &["defs", "helper"]);
    assert_eq!(
        current["results"][0]["location"]["source_status"], "current",
        "a compdb capture records a source hash, so the status is decided, \
         not `unknown`: {current}"
    );
    assert_eq!(current["uncertainty"]["locations_from_modified_sources"], 0);

    // Edit the source after capture: the same location must now say so.
    std::fs::write(
        &source,
        "int helper(int x){return x+2;}\nint main(void){return helper(1);}\n",
    )
    .unwrap();
    let modified = query_json(&scratch, &catalog, &["defs", "helper"]);
    assert_eq!(
        modified["results"][0]["location"]["source_status"], "modified",
        "{modified}"
    );
    assert!(
        modified["uncertainty"]["locations_from_modified_sources"]
            .as_u64()
            .unwrap()
            > 0,
        "the envelope must count the stale locations it just reported"
    );
}

#[test]
fn a_location_without_a_line_is_an_error_not_an_empty_answer() {
    // `indirect-targets parser.c` with the `:8` missing must not exit 0 with
    // `results: []`, which is byte-identical to a valid line that has no
    // indirect calls.
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(&scratch))
        .arg("--catalog")
        .arg(&catalog)
        .args(["indirect-targets", "main.c"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a location that does not parse must not answer: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty(), "no answer may reach stdout");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("main.c"),
        "the diagnostic must name the location: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_heuristics_flag_may_follow_its_subcommand() {
    // The README writes `indirect-targets parser.c:8 --heuristics`; without
    // `global = true` clap rejects that placement outright.
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let value = query_json(
        &scratch,
        &catalog,
        &["indirect-targets", "main.c:2", "--heuristics"],
    );
    assert_eq!(
        value["query"]["heuristics"], true,
        "the trailing flag must reach the query: {value}"
    );
}

/// Runs `rllvm-query --catalog <catalog> <args...>` and parses its stdout.
fn query_json(scratch: &tempfile::TempDir, catalog: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch))
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rllvm-query {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Two modules as plain `.bc` files rather than archive members, so one of
/// them can be removed from under a load that already verified it. Returns
/// the catalog and the path of the first module.
fn two_plain_module_catalog(scratch: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let mut modules = Vec::new();
    let mut origin = None;
    let mut first = None;
    for (name, source) in [
        ("add.c", "int add(int a,int b){return a+b;}\n"),
        (
            "main.c",
            "int add(int a,int b);\nint main(void){ return add(2,3); }\n",
        ),
    ] {
        let source_path = scratch.path().join(name);
        std::fs::write(&source_path, source).unwrap();
        let object = scratch.path().join(name.replace(".c", ".bc"));
        assert!(
            Command::new(llvm_bin("clang"))
                .args(["-g", "-O0", "-emit-llvm", "-c"])
                .arg(&source_path)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        let catalog =
            rllvm::catalog::inventory(&object, scratch.path(), Some(&llvm_bin("llvm-dis")))
                .unwrap();
        origin.get_or_insert(catalog.origin.clone());
        first.get_or_insert(object);
        modules.extend(catalog.modules);
    }
    let catalog = rllvm::catalog::ModuleCatalog::new(
        origin.expect("at least one module"),
        "recorded_modules",
        modules,
    );
    let path = scratch.path().join("plain-catalog.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    (path, first.expect("at least one module"))
}

#[test]
fn direct_calls_carry_caller_callee_and_line() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "int add(int a,int b){return a+b;}\n\
         int main(void){ return add(2,3); }\n",
    );
    let call = facts
        .call_sites
        .iter()
        .find(|c| matches!(&c.target, CallTarget::Direct { callee } if callee.symbol == "add"))
        .expect("direct call to add");
    assert_eq!(call.id.function.symbol, "main");
    assert_eq!(call.location.as_ref().unwrap().line, 2);
}

#[test]
fn two_calls_on_one_line_are_two_call_sites() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "int add(int a,int b){return a+b;}\n\
         int sub(int a,int b){return a-b;}\n\
         int main(void){ return add(1,2) + sub(3,4); }\n",
    );
    let line_three: Vec<_> = facts
        .call_sites
        .iter()
        .filter(|c| c.location.as_ref().is_some_and(|l| l.line == 3))
        .collect();
    assert_eq!(line_three.len(), 2);
    assert_ne!(line_three[0].id, line_three[1].id);
}

#[test]
fn a_function_whose_address_is_taken_records_a_use_not_a_call() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         int (*pick(void))(int,int){ return add; }\n",
    );
    assert!(
        facts.call_sites.iter().all(|c| !matches!(&c.target,
            CallTarget::Direct { callee } if callee.symbol == "add")),
        "taking an address is not a call"
    );
    assert!(facts.uses.iter().any(|u| u.used.symbol == "add"));
}

#[test]
fn a_function_without_debug_info_has_no_location() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(
        &scratch,
        "int add(int a,int b){return a+b;}\n",
        &["-O0"], // no -g
    );
    let add = facts
        .functions
        .iter()
        .find(|f| f.id.symbol == "add")
        .unwrap();
    assert!(add.location.is_none());
}

#[test]
fn a_module_from_a_newer_llvm_names_both_versions() {
    // A bitcode wrapper claiming a future producer version. Parsing fails;
    // the message is what this test pins.
    let loaded = rllvm::query::load::LoadedModule {
        id: "future".into(),
        bytes: b"BC\xc0\xde\xff\xff\xff\xff".to_vec(),
        record: record_with_compiler_version("clang 99.0.0"),
    };
    let error = rllvm::query::extract::extract(&loaded, &Default::default()).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("99.0.0"),
        "must name the producer: {message}"
    );
    assert!(
        message.contains(&rllvm::query::llvm_version()),
        "must name the reader: {message}"
    );
}

#[test]
fn an_ordinary_call_is_not_recorded_as_a_use() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "int add(int a,int b){return a+b;}\n\
         int main(void){ return add(2,3); }\n",
    );
    // Callee position is already a call site. Recording it again would make
    // every called function look address-taken to the heuristics that read
    // `uses`.
    assert!(
        facts.uses.iter().all(|u| u.used.symbol != "add"),
        "{:?}",
        facts.uses
    );
}

#[test]
fn mapped_lines_cover_every_line_a_function_has_instructions_on() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "int add(int a,int b){\n\
         \x20 int c = a + b;\n\
         \x20 return c;\n\
         }\n",
    );
    let add = facts
        .functions
        .iter()
        .find(|f| f.id.symbol == "add")
        .unwrap();
    let lines: Vec<u32> = add.mapped_lines.iter().map(|(_, line)| *line).collect();
    assert!(lines.contains(&2) && lines.contains(&3), "{lines:?}");
}

#[test]
fn a_parse_failure_carries_the_reason_llvm_gave() {
    // Without a diagnostic handler installed, LLVM's own report goes to
    // stderr instead of reaching the caller, and on the versions that call
    // `exit(1)` for an error it takes the process with it.
    let loaded = rllvm::query::load::LoadedModule {
        id: "broken".into(),
        bytes: b"BC\xc0\xde\xff\xff\xff\xff".to_vec(),
        record: Default::default(),
    };
    let message = rllvm::query::extract::extract(&loaded, &Default::default())
        .unwrap_err()
        .to_string();
    // The severity-tagged report LLVM produced, not the fixed template around
    // it. Its wording belongs to LLVM and is not pinned here; that it arrived
    // at all is.
    let reported = message
        .split_once('(')
        .and_then(|(_, rest)| rest.rsplit_once(')'))
        .map(|(inside, _)| inside)
        .unwrap_or_default();
    assert!(
        reported.starts_with("error: ") && reported.len() > "error: ".len(),
        "LLVM's own report must reach the error: {message}"
    );
}

#[test]
fn an_inlined_frame_carries_its_own_file_not_the_leaf_s() {
    let scratch = tempfile::tempdir().unwrap();
    // `add` is inlined into `main`, so the call to `outer` sits in `main`
    // with a leaf location in the header and a frame above it in `t.c`. Each
    // frame resolves through its own scope; copying the leaf's file upward
    // would report the header twice.
    std::fs::write(
        scratch.path().join("helper.h"),
        "int outer(int);\n\
         \n\
         static inline __attribute__((always_inline)) int add(int a,int b){ return outer(a+b); }\n",
    )
    .unwrap();
    std::fs::write(
        scratch.path().join("t.c"),
        "#include \"helper.h\"\n\
         int main(void){ return add(2,3); }\n",
    )
    .unwrap();
    let facts = compile_and_extract(&scratch, &["-g", "-O1"]);

    let call = facts
        .call_sites
        .iter()
        .find(|c| matches!(&c.target, CallTarget::Direct { callee } if callee.symbol == "outer"))
        .expect("call to outer");
    let leaf = call
        .location
        .as_ref()
        .expect("an inlined call has a location");
    assert_eq!(leaf.file.file_name().unwrap(), "helper.h");
    assert_eq!(leaf.line, 3);

    let frame = leaf
        .inlined_at
        .first()
        .unwrap_or_else(|| panic!("inlining chain is empty for {leaf:?}"));
    assert_eq!(
        frame.file.file_name().unwrap(),
        "t.c",
        "the frame above the leaf is in the caller's file: {frame:?}"
    );
    assert_eq!(frame.line, 2);
}

#[test]
fn a_signature_records_parameter_and_return_types() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(&scratch, "int add(int a,int b){return a+b;}\n");
    let add = facts
        .functions
        .iter()
        .find(|f| f.id.symbol == "add")
        .unwrap();
    // The function's value type. Under opaque pointers the type *of* the
    // value is `ptr`, which records no signature at all.
    assert!(
        add.signature.starts_with("i32 (") && add.signature.contains("i32, i32"),
        "{}",
        add.signature
    );
}

/// From the spec's verified constraint 2. Each row is a CVP eligibility rule.
#[test]
fn cvp_bounds_an_internal_global_at_o0() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         static int (*fp)(int,int) = add;\n\
         int pick(int x){ if(x) fp = sub; return fp(2,3); }\n",
    );
    let bound = indirect_bound(&facts);
    let mut names: Vec<_> = bound
        .expect("internal global is eligible")
        .iter()
        .map(|f| f.symbol.clone())
        .collect();
    names.sort();
    assert_eq!(names, vec!["add", "sub"]);
}

#[test]
fn cvp_does_not_bound_an_external_global() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         int (*fp)(int,int) = add;\n\
         int pick(int x){ if(x) fp = sub; return fp(2,3); }\n",
    );
    assert_unresolved_indirect_site(&facts, "external linkage is ineligible");
}

#[test]
fn cvp_does_not_bound_a_stack_slot_at_o0() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         __attribute__((noinline)) static int apply(int(*f)(int,int),int x){ return f(x,3); }\n\
         int run(int x){ return apply(add,x) + apply(sub,x); }\n",
    );
    assert_unresolved_indirect_site(&facts, "-O0 routes the argument through a stack slot");
}

#[test]
fn cvp_bounds_the_same_argument_at_o1() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         __attribute__((noinline)) static int apply(int(*f)(int,int),int x){ return f(x,3); }\n\
         int run(int x){ return apply(add,x) + apply(sub,x); }\n",
        &["-g", "-O1"],
    );
    assert_eq!(indirect_bound(&facts).map(|b| b.len()), Some(2));
}

/// From the spec's verified-constraints table: a struct field, the fourth
/// eligibility row alongside the internal global, the external global, and
/// the `noinline` argument above.
#[test]
fn cvp_does_not_bound_a_struct_field_at_o0() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         struct ops { int (*op)(int,int); };\n\
         int run(int x){ struct ops o; o.op = x ? add : sub; return o.op(1,2); }\n",
    );
    assert_unresolved_indirect_site(
        &facts,
        "-O0 leaves the field in an alloca CVP cannot follow",
    );
}

#[test]
fn cvp_bounds_the_same_struct_field_at_o1() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(
        &scratch,
        "static int add(int a,int b){return a+b;}\n\
         static int sub(int a,int b){return a-b;}\n\
         struct ops { int (*op)(int,int); };\n\
         int run(int x){ struct ops o; o.op = x ? add : sub; return o.op(1,2); }\n",
        &["-g", "-O1"],
    );
    let mut names: Vec<_> = indirect_bound(&facts)
        .expect("SROA promotes the field to SSA at -O1")
        .iter()
        .map(|f| f.symbol.clone())
        .collect();
    names.sort();
    assert_eq!(names, vec!["add", "sub"]);
}

#[test]
fn cvp_bounds_a_four_candidate_set() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(&scratch, &switch_dispatch_source(4));
    let mut names: Vec<_> = indirect_bound(&facts)
        .expect("four candidates is within CVP's default limit")
        .iter()
        .map(|f| f.symbol.clone())
        .collect();
    names.sort();
    assert_eq!(names, vec!["f1", "f2", "f3", "f4"]);
}

#[test]
fn cvp_drops_candidate_sets_above_four() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(&scratch, &switch_dispatch_source(5));
    assert_unresolved_indirect_site(&facts, "five candidates exceed CVP's default limit of four");
}

#[test]
fn cvp_does_not_propagate_across_modules() {
    // The callback is registered in one translation unit and invoked in
    // another. CVP runs per module, so the invoking module cannot see the
    // assignment and the site stays unresolved. Expressed at the extraction
    // layer: Session and bindings do not exist yet, and would not change the
    // answer if they did.
    let scratch = tempfile::tempdir().unwrap();

    let registering = extract_source(
        &scratch,
        "int handle(int a,int b){return a+b;}\n\
         extern void install(int(*)(int,int));\n\
         void setup(void){ install(handle); }\n",
    );
    assert!(registering.uses.iter().any(|u| u.used.symbol == "handle"));

    let invoking = extract_source(
        &scratch,
        "static int (*slot)(int,int);\n\
         void install(int(*f)(int,int)){ slot = f; }\n\
         int fire(void){ return slot(1,2); }\n",
    );
    assert_unresolved_indirect_site(
        &invoking,
        "the only assignment lives in another module, so no bound is possible",
    );
}

/// A `switch` dispatching one of `candidates` internal functions through one
/// internal global, called once at the end. Shared by the tests pinning
/// CVP's default candidate-set limit from both sides.
fn switch_dispatch_source(candidates: usize) -> String {
    let mut source = String::new();
    for index in 1..=candidates {
        source.push_str(&format!(
            "static int f{index}(int a,int b){{return a+{index}*b;}}\n"
        ));
    }
    source.push_str("static int (*fp)(int,int) = f1;\n");
    source.push_str("int pick(int x){ switch(x){");
    for index in 1..=candidates {
        source.push_str(&format!(" case {index}: fp = f{index}; break;"));
    }
    source.push_str(" } return fp(2,3); }\n");
    source
}

/// The `CallTarget::Indirect` a fixture is expected to have exactly one of.
/// Panics if none is found, so a fixture that stops producing an indirect
/// call site fails loudly instead of letting every `None`-expecting
/// assertion downstream pass vacuously.
fn indirect_target(facts: &rllvm::query::ModuleFacts) -> &CallTarget {
    facts
        .call_sites
        .iter()
        .map(|site| &site.target)
        .find(|target| matches!(target, CallTarget::Indirect { .. }))
        .expect("fixture must contain an indirect call site")
}

fn indirect_bound(facts: &rllvm::query::ModuleFacts) -> Option<Vec<rllvm::query::FunctionId>> {
    match indirect_target(facts) {
        CallTarget::Indirect {
            llvm_target_bound, ..
        } => llvm_target_bound.clone(),
        _ => unreachable!("indirect_target only ever returns an Indirect target"),
    }
}

/// Asserts the fixture's one indirect call site exists and carries no
/// `llvm_target_bound`. Stronger than asserting `indirect_bound(facts).is_none()`
/// alone: that alone would pass just as well if the fixture recorded no
/// indirect call site at all.
fn assert_unresolved_indirect_site(facts: &rllvm::query::ModuleFacts, message: &str) {
    let target = indirect_target(facts);
    assert!(
        matches!(
            target,
            CallTarget::Indirect {
                llvm_target_bound: None,
                ..
            }
        ),
        "{message}: {target:?}"
    );
}

fn extract_source(scratch: &tempfile::TempDir, source: &str) -> rllvm::query::ModuleFacts {
    extract_source_with_flags(scratch, source, &["-g", "-O0"])
}

fn extract_source_with_flags(
    scratch: &tempfile::TempDir,
    source: &str,
    flags: &[&str],
) -> rllvm::query::ModuleFacts {
    std::fs::write(scratch.path().join("t.c"), source).unwrap();
    compile_and_extract(scratch, flags)
}

/// Compiles the `t.c` already written into `scratch`, so a fixture can put a
/// header beside it first.
fn compile_and_extract(scratch: &tempfile::TempDir, flags: &[&str]) -> rllvm::query::ModuleFacts {
    let path = scratch.path().join("t.c");
    let module = scratch.path().join("t.bc");
    let status = std::process::Command::new(llvm_bin("clang"))
        .args(flags)
        .args(["-emit-llvm", "-c"])
        .arg(&path)
        .arg("-o")
        .arg(&module)
        .status()
        .unwrap();
    assert!(status.success());
    let loaded = rllvm::query::load::LoadedModule {
        id: "t".into(),
        bytes: std::fs::read(&module).unwrap(),
        record: Default::default(),
    };
    rllvm::query::extract::extract(&loaded, &Default::default()).unwrap()
}

/// Stands in for a catalog whose capture recorded a compiler this LLVM is
/// older than.
fn record_with_compiler_version(version: &str) -> rllvm::catalog::ModuleRecord {
    rllvm::catalog::ModuleRecord {
        compiler: Some(rllvm::catalog::CompilerIdentity {
            path: PathBuf::from("clang"),
            realpath: None,
            version: version.to_string(),
            sha256: None,
        }),
        ..Default::default()
    }
}

// --- MCP stdio server -------------------------------------------------
//
// Nested in its own module so `cargo test --features query --test query mcp`
// selects exactly this group by path.
//
// Every modern message shape below is copied from the official schema
// examples at `schema/2026-07-28/examples/` in
// `modelcontextprotocol/modelcontextprotocol` (`ListToolsRequest`,
// `DiscoverRequest`, `CallToolRequest`, `ListToolsResultResponse`,
// `DiscoverResultResponse`), not written from memory: `_meta` lives inside
// `params`, the key is `io.modelcontextprotocol/protocolVersion` in
// camelCase, and `io.modelcontextprotocol/clientCapabilities` is required on
// every modern request.
mod mcp {
    use super::*;

    const MODERN: &str = "2026-07-28";
    const LEGACY: &str = "2025-06-18";

    /// The `_meta` block every modern request must carry.
    fn modern_meta() -> serde_json::Value {
        serde_json::json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientInfo": { "name": "rllvm-test", "version": "1.0.0" },
            "io.modelcontextprotocol/clientCapabilities": {}
        })
    }

    fn modern_request(id: &str, method: &str, mut params: serde_json::Value) -> String {
        params["_meta"] = modern_meta();
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            .to_string()
    }

    /// An empty catalog: no modules to load or extract, just enough for
    /// `build_session` to produce a `Session` the server can answer over. The
    /// MCP tests below only need a session to exist, not any particular
    /// program in it.
    fn empty_catalog(scratch: &tempfile::TempDir) -> PathBuf {
        let catalog = rllvm::catalog::ModuleCatalog::new(
            rllvm::catalog::CatalogOrigin {
                kind: "test".into(),
                input: PathBuf::from("test"),
                sha256: None,
            },
            "test",
            vec![],
        );
        let catalog_path = scratch.path().join("catalog.json");
        rllvm::catalog::write_catalog(&catalog_path, &catalog).unwrap();
        catalog_path
    }

    /// Spawns `rllvm-query --catalog <catalog> mcp` against an existing
    /// scratch/catalog pair, writes one request line to its stdin, closes
    /// stdin so the server sees EOF and exits, and returns everything the
    /// process wrote to stdout, raw. Shared by `mcp_raw` (a fresh, unused
    /// catalog per call) and tests that need several calls to see the same
    /// session.
    fn mcp_raw_in(scratch: &tempfile::TempDir, catalog: &Path, request: &str) -> String {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
            .env("RLLVM_CONFIG", scratch_rllvm_config(scratch))
            .arg("--catalog")
            .arg(catalog)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        // The transport is one JSON-RPC frame per line. A fixture may format
        // its JSON across multiple source lines for readability (e.g. the
        // legacy `initialize` request); JSON's grammar treats a newline
        // between tokens as insignificant whitespace, so collapsing it to a
        // space here changes nothing the server parses.
        let single_line: String = request
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();

        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{single_line}").unwrap();
        drop(stdin);

        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "rllvm-query mcp exited with an error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Sends one request against an existing scratch/catalog pair and parses
    /// the server's one response line as JSON.
    fn mcp_exchange_in(
        scratch: &tempfile::TempDir,
        catalog: &Path,
        request: &str,
    ) -> serde_json::Value {
        let raw = mcp_raw_in(scratch, catalog, request);
        let line = raw
            .lines()
            .find(|line| !line.is_empty())
            .unwrap_or_else(|| panic!("no response line in: {raw:?}"));
        serde_json::from_str(line).unwrap()
    }

    /// `mcp_raw_in` against a fresh, unused empty catalog: for tests that
    /// only need one exchange and don't care what session answered it.
    fn mcp_raw(request: &str) -> String {
        let scratch = tempfile::tempdir().unwrap();
        let catalog = empty_catalog(&scratch);
        mcp_raw_in(&scratch, &catalog, request)
    }

    /// `mcp_exchange_in` against a fresh, unused empty catalog.
    fn mcp_exchange(request: &str) -> serde_json::Value {
        let scratch = tempfile::tempdir().unwrap();
        let catalog = empty_catalog(&scratch);
        mcp_exchange_in(&scratch, &catalog, request)
    }

    /// Calls one tool against `catalog` over `tools/call` and returns the
    /// decoded envelope (`results`/`analysis`/`uncertainty`) the query
    /// produced, not the `CallToolResult` wrapper around it: `tool_call_outcome`
    /// (`mcp.rs`) serializes `run`'s result to a JSON string and carries it in
    /// `content[0].text`, so this unwraps that one layer for callers that want
    /// to compare it against the CLI's own JSON on stdout.
    fn mcp_tool_call(
        catalog: &Path,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let scratch = tempfile::tempdir().unwrap();
        let response = mcp_exchange_in(
            &scratch,
            catalog,
            &modern_request(
                "call",
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            ),
        );
        assert_eq!(
            response["result"]["isError"], false,
            "tool `{name}` call failed: {response:?}"
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_cli_and_mcp_return_the_same_answer() {
        // Structural, not a bug hunt: the CLI and `tools/call` both resolve
        // to `run`/`Session` (`mcp.rs`'s `tool_call_outcome` calls the same
        // `run` the CLI's own dispatch calls), so this documents that
        // invariant rather than searching for a place the two diverge.
        let scratch = tempfile::tempdir().unwrap();
        let catalog = super::two_module_catalog(&scratch); // main calls add

        let cli: serde_json::Value = serde_json::from_slice(
            &Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
                .env("RLLVM_CONFIG", super::scratch_rllvm_config(&scratch))
                .arg("--catalog")
                .arg(&catalog)
                .args(["callers", "add"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();

        let mcp = mcp_tool_call(&catalog, "callers", serde_json::json!({ "name": "add" }));

        assert_eq!(cli["results"], mcp["results"]);
        assert_eq!(cli["analysis"], mcp["analysis"]);
        assert_eq!(cli["uncertainty"], mcp["uncertainty"]);
    }

    #[test]
    fn a_modern_request_is_served_without_a_handshake() {
        let response = mcp_exchange(&modern_request(
            "list-tools",
            "tools/list",
            serde_json::json!({}),
        ));
        assert_eq!(response["result"]["resultType"], "complete");
        assert!(response["result"]["tools"].as_array().unwrap().len() >= 9);
    }

    #[test]
    fn server_discover_returns_supported_versions() {
        let response = mcp_exchange(&modern_request(
            "discover",
            "server/discover",
            serde_json::json!({}),
        ));
        let result = &response["result"];
        assert_eq!(result["resultType"], "complete");
        let versions = result["supportedVersions"].as_array().unwrap();
        assert!(versions.iter().any(|v| v == MODERN));
        assert!(
            versions.iter().any(|v| v == LEGACY),
            "a dual-era server supports both"
        );
        assert!(result["capabilities"]["tools"].is_object());
        assert!(
            result["ttlMs"].is_number(),
            "DiscoverResult extends CacheableResult"
        );
    }

    #[test]
    fn a_modern_tool_call_returns_a_call_tool_result() {
        let response = mcp_exchange(&modern_request(
            "call",
            "tools/call",
            serde_json::json!({ "name": "externals", "arguments": {} }),
        ));
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["content"].as_array().is_some());
    }

    #[test]
    fn every_listed_tool_resolves_through_a_call() {
        // `tool_for`'s "name" literals (what `tools/list` reports) and
        // `query_from_call`'s match arms (what `tools/call` actually
        // recognizes) are two independent lists in `mcp.rs`: nothing forces
        // them to agree. Renaming one and not the other compiles cleanly and
        // makes that tool permanently uncallable -- every invocation would
        // return `isError: true, unknown tool`. This drives every name
        // `tools/list` reports through `tools/call` with a valid argument
        // object, which also exercises the argument plumbing for the eight
        // tools the other `tools/call` test (`externals`, no arguments)
        // does not.
        let scratch = tempfile::tempdir().unwrap();
        let catalog = empty_catalog(&scratch);

        let list = mcp_exchange_in(
            &scratch,
            &catalog,
            &modern_request("list-tools", "tools/list", serde_json::json!({})),
        );
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 9);

        let sample_arguments: std::collections::HashMap<&str, serde_json::Value> = [
            ("defs", serde_json::json!({ "name": "f" })),
            ("at", serde_json::json!({ "file": "t.c", "line": 1 })),
            ("callers", serde_json::json!({ "name": "f" })),
            ("callees", serde_json::json!({ "name": "f" })),
            ("uses", serde_json::json!({ "name": "f" })),
            ("reach", serde_json::json!({ "from": "a", "to": "b" })),
            (
                "closure",
                serde_json::json!({ "name": "f", "direction": "in" }),
            ),
            ("externals", serde_json::json!({})),
            ("indirect_targets", serde_json::json!({ "at": "t.c:4" })),
        ]
        .into_iter()
        .collect();

        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let arguments = sample_arguments
                .get(name)
                .unwrap_or_else(|| panic!("no sample arguments for listed tool `{name}`"));
            let response = mcp_exchange_in(
                &scratch,
                &catalog,
                &modern_request(
                    "call",
                    "tools/call",
                    serde_json::json!({ "name": name, "arguments": arguments }),
                ),
            );
            assert_eq!(
                response["result"]["isError"], false,
                "tool `{name}` did not resolve through query_from_call: {response:?}"
            );
        }
    }

    #[test]
    fn a_request_without_client_capabilities_is_rejected() {
        // `clientCapabilities` is required; accepting its absence would make the
        // server pass tests a conforming client would fail against.
        let response = mcp_exchange(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list",
                "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } }
            })
            .to_string(),
        );
        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn an_unsupported_version_returns_minus_32022_with_the_supported_list() {
        let response = mcp_exchange(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list",
                "params": { "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            })
            .to_string(),
        );
        assert_eq!(response["error"]["code"], -32022);
        let supported = response["error"]["data"]["supported"].as_array().unwrap();
        assert!(supported.iter().any(|v| v == MODERN));
    }

    #[test]
    fn a_legacy_initialize_selects_legacy_semantics() {
        let response = mcp_exchange(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize",
             "params":{{"protocolVersion":"{LEGACY}","capabilities":{{}},
             "clientInfo":{{"name":"t","version":"1"}}}}}}"#
        ));
        assert_eq!(response["result"]["protocolVersion"], LEGACY);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert!(
            response["result"]["resultType"].is_null(),
            "legacy results must not carry modern fields"
        );
    }

    #[test]
    fn a_legacy_initialize_with_an_older_version_gets_a_counter_offer() {
        // The 2025-06-18 lifecycle spec: "If the server supports the
        // requested protocol version, it MUST respond with the same
        // version. Otherwise, the server MUST respond with another protocol
        // version it supports." That is a successful `InitializeResult`
        // naming `LEGACY`, not a protocol error -- a legacy client has no
        // fall-forward mechanism, so an error would be exactly the dead end
        // the counter-offer exists to avoid.
        let response = mcp_exchange(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize",
             "params":{"protocolVersion":"2024-11-05","capabilities":{},
             "clientInfo":{"name":"t","version":"1"}}}"#,
        );
        assert!(
            response["error"].is_null(),
            "an unsupported requested version must counter-offer, not error: {response:?}"
        );
        assert_eq!(response["result"]["protocolVersion"], LEGACY);
    }

    #[test]
    fn malformed_json_returns_minus_32700() {
        let response = mcp_exchange("{not json");
        assert_eq!(response["error"]["code"], -32700);
    }

    #[test]
    fn stdout_carries_only_protocol_frames() {
        let raw = mcp_raw(&modern_request(
            "list-tools",
            "tools/list",
            serde_json::json!({}),
        ));
        for line in raw.lines().filter(|l| !l.is_empty()) {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| panic!("non-protocol output on stdout: {line}"));
        }
    }
} // mod mcp
