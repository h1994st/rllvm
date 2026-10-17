use std::{
    collections::{BTreeSet, HashMap},
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use rllvm_core::catalog::read_catalog;
use rllvm_query::load::{for_each_module, load_catalog};
use rllvm_query::{CallTarget, Language, LanguageBasis, Linkage, SourceLanguage};
use rllvm_testkit::{
    MODULE_ID, SourceFixture, compile_bitcode, compile_bitcode_file, llvm_bin,
    scratch_rllvm_config, source_and_header, write_catalog_json,
};

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
fn query_binary_reports_its_own_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "--version failed: {output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rllvm-query {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// Completions come from the binary that owns the CLI.
///
/// `rllvm-completions` lives in the wrapper crate and cannot see `QueryArgs`
/// without dragging LLVM into every wrapper build, so each installable unit
/// generates its own.
#[test]
fn completions_name_the_query_binary() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .args(["completions", "bash"])
        .output()
        .expect("failed to run rllvm-query");
    assert!(output.status.success(), "completions failed");
    let script = String::from_utf8_lossy(&output.stdout);
    assert!(
        script.contains("rllvm-query"),
        "completion script does not name the binary: {script:.200}"
    );
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
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["--json", "callers", "add"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["results"][0]["function"]["symbol"], "main");
}

/// Text is the default now: `--json` is required to get the envelope, not
/// implied by running the binary at all.
#[test]
fn the_cli_prints_text_by_default() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["callers", "add"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.trim_start().starts_with('{'), "got JSON: {text}");
    assert!(text.contains("main"), "got: {text}");
}

/// `--full` only makes sense for the text renderer; combined with `--json`
/// neither flag would silently win, so clap rejects the combination outright.
#[test]
fn json_and_full_cannot_be_combined() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["--json", "--full", "callers", "add"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "clap should reject the combination"
    );
}

const ADD_AND_MAIN: &[(&str, &str)] = &[
    ("add.c", "int add(int a,int b){return a+b;}\n"),
    (
        "main.c",
        "int add(int a,int b);\nint main(void){ return add(2,3); }\n",
    ),
];

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
        rllvm_query::ModuleAnalysis::Changed
    );
    assert_eq!(
        loaded.scope.selected_entries,
        read_catalog(&catalog_path).unwrap().scope.selected_entries,
        "scope must not shrink when a module fails verification"
    );
}

/// The state the loader derives for one of the fixture's files. Stays here
/// rather than in `rllvm-testkit`: `SourceStatus` belongs to this crate,
/// while the digests it reads do not.
fn source_state(fixture: &SourceFixture, file: &Path) -> rllvm_query::load::SourceState {
    load_catalog(&fixture.catalog)
        .unwrap()
        .source_status
        .get(&(MODULE_ID.to_string(), file.to_path_buf()))
        .copied()
        .unwrap_or_else(|| panic!("no association recorded for {}", file.display()))
}

fn source_status(fixture: &SourceFixture, file: &Path) -> rllvm_query::SourceStatus {
    source_state(fixture, file).status
}

/// The case the whole change exists for: the source is untouched and only the
/// header changed. Every module that included it has stale line numbers, and
/// before this nothing could say so.
#[test]
fn editing_only_the_header_marks_the_header_modified_and_leaves_the_source_current() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = source_and_header(&scratch);

    assert_eq!(
        source_status(&fixture, &fixture.source),
        rllvm_query::SourceStatus::Current
    );
    assert_eq!(
        source_status(&fixture, &fixture.header),
        rllvm_query::SourceStatus::Current
    );

    std::fs::write(&fixture.header, "int helper(int x){return x+2;}\n").unwrap();

    assert_eq!(
        source_status(&fixture, &fixture.header),
        rllvm_query::SourceStatus::Modified,
        "the edited header must be reported, not just the translation unit"
    );
    assert_eq!(
        source_status(&fixture, &fixture.source),
        rllvm_query::SourceStatus::Current,
        "and the untouched source must not be dragged along with it"
    );
}

/// The same edit, seen through a query answer rather than the loader: a
/// location in the header carries `modified`, so a reader is told the line
/// numbers may no longer apply.
#[test]
fn a_location_in_an_edited_header_answers_modified() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = source_and_header(&scratch);
    std::fs::write(&fixture.header, "int helper(int x){return x+2;}\n").unwrap();

    let answer = query_json(&scratch, &fixture.catalog, &["defs", "helper"]);
    let location = &answer["results"][0]["location"];
    assert_eq!(
        location["source_status"], "modified",
        "the definition lives in the edited header: {answer}"
    );
    assert_eq!(
        location["status_basis"], "compiler",
        "and the digest came from the compiler, so this is a claim about the bitcode"
    );

    let main = query_json(&scratch, &fixture.catalog, &["defs", "main"]);
    assert_eq!(
        main["results"][0]["location"]["source_status"], "current",
        "while the untouched translation unit stays current"
    );
}

/// The whole of #186 in one test: a catalog from the inventory path -- what
/// `rllvm-get-bc` writes -- now decides staleness on its own, with no hash
/// stamped in by hand.
/// A catalog from the inventory path -- what `rllvm-get-bc` writes -- now
/// decides staleness on its own, with no hash stamped in by hand, and the
/// three outcomes stay distinct.
#[test]
fn an_inventoried_source_is_current_until_it_is_edited_then_missing() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = source_and_header(&scratch);

    let state = source_state(&fixture, &fixture.source);
    assert_eq!(state.status, rllvm_query::SourceStatus::Current);
    assert_eq!(
        state.basis,
        Some(rllvm_core::catalog::DigestOrigin::Compiler),
        "clang records the digest in !DIFile, so this is a claim about the bitcode"
    );

    std::fs::write(&fixture.source, "int main(void){return 0;}\n").unwrap();
    assert_eq!(
        source_status(&fixture, &fixture.source),
        rllvm_query::SourceStatus::Modified
    );

    std::fs::remove_file(&fixture.source).unwrap();
    let state = source_state(&fixture, &fixture.source);
    assert_eq!(state.status, rllvm_query::SourceStatus::Missing);
    assert_eq!(state.basis, None, "nothing was compared");
}

/// An association carrying no digest still answers `unknown` rather than
/// guessing, so `unknown` keeps meaning "cannot tell" and not "unchecked".
#[test]
fn a_source_without_a_recorded_digest_is_unknown_not_modified() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = source_and_header(&scratch);
    assert!(
        fixture.digest(&fixture.header).is_some(),
        "the fixture must start with a digest, or this proves nothing"
    );
    strip_source_digests(&fixture.catalog);
    assert!(fixture.digest(&fixture.header).is_none(), "strip failed");
    std::fs::write(&fixture.header, "int helper(int x){return x+9;}\n").unwrap();

    let state = source_state(&fixture, &fixture.header);
    assert_eq!(state.status, rllvm_query::SourceStatus::Unknown);
    assert_eq!(state.basis, None);
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
            .map(|state| state.status),
        Some(rllvm_query::SourceStatus::Current)
    );
    assert_eq!(
        loaded
            .source_status
            .get(&("stale".to_string(), source))
            .map(|state| state.status),
        Some(rllvm_query::SourceStatus::Modified),
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
            .all(|report| report.status == rllvm_query::ModuleAnalysis::Missing),
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
    let module = compile_bitcode(scratch, "add.c", "int add(int a,int b){return a+b;}\n");
    let mut catalog =
        rllvm_core::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis")))
            .unwrap();
    catalog.modules[0].id = "add".to_string();
    let catalog_path = scratch.path().join("catalog.json");
    write_catalog_json(&catalog_path, &catalog);
    (catalog_path, module)
}

/// Removes every recorded digest, standing in for a capture that could not
/// establish one.
fn strip_source_digests(catalog_path: &Path) {
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(catalog_path).unwrap()).unwrap();
    for module in catalog["modules"].as_array_mut().unwrap() {
        for association in module["sources"].as_array_mut().unwrap() {
            association["digest"] = serde_json::Value::Null;
        }
    }
    std::fs::write(catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
}

/// Builds a catalog with two modules, "fresh" and "stale", that both record a
/// source association with one shared source file. "fresh" is stamped with
/// the source's real current hash; "stale" is stamped with a hash that does
/// not match, standing in for a capture whose source has since changed.
fn two_modules_one_source(scratch: &tempfile::TempDir) -> PathBuf {
    let module = compile_bitcode(scratch, "shared.c", "int shared(void){return 0;}\n");
    let source = scratch.path().join("shared.c");

    let base = rllvm_core::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis")))
        .unwrap();
    let template = base.modules[0].clone();

    let digest = |bytes: &[u8]| rllvm_core::catalog::SourceDigest {
        algorithm: rllvm_core::catalog::DigestAlgorithm::Sha256,
        value: rllvm_core::catalog::hash_bytes(bytes),
        origin: rllvm_core::catalog::DigestOrigin::Capture,
    };
    let current = digest(&std::fs::read(&source).unwrap());
    let stale_digest = digest(b"stale content, does not match shared.c");

    let mut fresh = template.clone();
    fresh.id = "fresh".to_string();
    for association in &mut fresh.sources {
        association.digest = Some(current.clone());
    }

    let mut stale = template;
    stale.id = "stale".to_string();
    for association in &mut stale.sources {
        association.digest = Some(stale_digest.clone());
    }

    let catalog = rllvm_core::catalog::ModuleCatalog::new(
        base.origin,
        "recorded_modules",
        vec![fresh, stale],
    );
    let path = scratch.path().join("shared-catalog.json");
    write_catalog_json(&path, &catalog);
    path
}

/// Compiles every `(name, source)` into one archive named `<stem>.a` and
/// inventories it, so the catalog carries genuine content hashes and
/// `archive_member` indices, and exercises the archive path the loader
/// already supports.
fn archive_catalog_of(
    scratch: &tempfile::TempDir,
    stem: &str,
    sources: &[(&str, &str)],
) -> PathBuf {
    let objects: Vec<PathBuf> = sources
        .iter()
        .map(|(name, source)| compile_bitcode(scratch, name, source))
        .collect();
    let archive = scratch.path().join(format!("{stem}.a"));
    assert!(
        Command::new(llvm_bin("llvm-ar"))
            .arg("rs")
            .arg(&archive)
            .args(&objects)
            .status()
            .unwrap()
            .success()
    );
    let catalog =
        rllvm_core::catalog::inventory(&archive, scratch.path(), Some(&llvm_bin("llvm-dis")))
            .unwrap();
    let path = scratch.path().join(format!("{stem}-catalog.json"));
    write_catalog_json(&path, &catalog);
    path
}

/// Two unrelated modules in one archive, for the loader's archive-member
/// handling. The archive is `lib.a`, which those tests delete by name.
fn archive_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    archive_catalog_of(
        scratch,
        "lib",
        &[
            ("one.c", "int one(void){return 0;}\n"),
            ("two.c", "int two(void){return 0;}\n"),
        ],
    )
}

/// `main` calls `add`, across two modules in one archive.
fn two_module_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    archive_catalog_of(scratch, "two", ADD_AND_MAIN)
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
fn a_module_rewritten_after_loading_is_unreadable_not_parsed() {
    // Valid bitcode with different bytes: only the hash check can tell. A
    // cache keyed by the recorded hash must never store what these bytes say.
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, first_module) = two_plain_module_catalog(&scratch);
    let loaded = load_catalog(&catalog_path).unwrap();
    let rewritten = loaded
        .pending
        .iter()
        .find(|module| module.path.file_name() == first_module.file_name())
        .unwrap()
        .id
        .clone();

    let other = compile_bitcode(&scratch, "other.c", "int other(void){return 1;}\n");
    std::fs::copy(&other, &first_module).unwrap();

    let mut visited = Vec::new();
    let unreadable = for_each_module(&loaded, |module| {
        visited.push(module.id.clone());
        Ok(())
    })
    .unwrap();

    assert!(
        !visited.contains(&rewritten),
        "rewritten bytes were handed over"
    );
    assert_eq!(unreadable.len(), 1);
    assert_eq!(unreadable[0].0, rewritten);
    assert!(
        unreadable[0].1.to_string().contains("changed"),
        "{}",
        unreadable[0].1
    );
}

/// `rllvm-compdb` belongs to the `rllvm` package, so cargo sets no
/// `CARGO_BIN_EXE_rllvm-compdb` for this test binary -- that variable only
/// ever names the current package's own binaries. Both packages share one
/// target directory, so it sits beside this crate's binary once it is built.
///
/// Asserted rather than skipped: a test that quietly does nothing when a
/// prerequisite is missing is indistinguishable from a passing one.
fn compdb_binary() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_rllvm-query"));
    path.set_file_name("rllvm-compdb");
    assert!(
        path.is_file(),
        "{} is missing: this test drives the compilation-database tool from the \
         sibling package, so build it first with `cargo build -p rllvm --bin rllvm-compdb`",
        path.display()
    );
    path
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

    let generate = Command::new(compdb_binary())
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
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
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
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

#[test]
fn a_reader_that_closes_stdout_early_is_not_an_error() {
    // `rllvm-query ... | head` closes the pipe before the answer is written.
    // The reader dropped here before the spawn makes that deterministic.
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    for format in [None, Some("--json")] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
            .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
            .arg("--catalog")
            .arg(&catalog)
            .args(format)
            .args(["callers", "add"])
            .stdout(writer)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{format:?}; stderr: {stderr}");
        assert!(stderr.is_empty(), "{format:?}; stderr: {stderr}");
    }
}

/// Runs `rllvm-query --catalog <catalog> <flags>` with `input` on stdin.
fn query_stdin(
    scratch: &tempfile::TempDir,
    catalog: &Path,
    flags: &[&str],
    input: &str,
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(catalog)
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn query_text(scratch: &tempfile::TempDir, catalog: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "rllvm-query {args:?} failed");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn queries_piped_on_stdin_answer_as_their_separate_invocations_would() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = query_stdin(&scratch, &catalog, &[], "callers add\n\ncallees main\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = format!(
        "== callers add\n{}== callees main\n{}",
        query_text(&scratch, &catalog, &["callers", "add"]),
        query_text(&scratch, &catalog, &["callees", "main"]),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn queries_piped_on_stdin_print_one_json_envelope_per_line() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    // Every invocation below shares this scratch config's cache directory.
    // Warming it first means each one reports the same steady-state
    // `analysis.cache` (a hit for every module) rather than whichever
    // happens to run first paying the miss and the rest hitting behind it.
    query_json(&scratch, &catalog, &["externals"]);
    let output = query_stdin(
        &scratch,
        &catalog,
        &["--json"],
        "callers add\ncallees main\n",
    );
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let answers: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        answers,
        [
            query_json(&scratch, &catalog, &["callers", "add"]),
            query_json(&scratch, &catalog, &["callees", "main"]),
        ]
    );
}

#[test]
fn a_bad_stdin_line_fails_before_the_catalog_is_read() {
    // The catalog does not exist: an error naming the line, not the missing
    // file, proves every line was checked before anything was opened.
    let scratch = tempfile::tempdir().unwrap();
    let catalog = scratch.path().join("absent.json");
    let output = query_stdin(
        &scratch,
        &catalog,
        &[],
        "callers add\nindirect-targets main.c\n",
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "no answer may reach stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("line 2:"), "stderr: {stderr}");
}

#[test]
fn empty_stdin_answers_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = query_stdin(&scratch, &catalog, &[], "");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

/// `main` calls `add` across two archive members; `sub` and `unused` sit in
/// those same modules, off the path.
const SLICE_SOURCES: &[(&str, &str)] = &[
    (
        "add.c",
        "int add(int a,int b){return a+b;}\nint sub(int a,int b){return a-b;}\n",
    ),
    (
        "main.c",
        "int add(int a,int b);\nint unused(void){return 7;}\nint main(void){ return add(2,3); }\n",
    ),
];

/// Every symbol a module defines, read back through extraction.
fn defined_symbols(module: &Path) -> BTreeSet<String> {
    extract_module(module)
        .functions
        .into_iter()
        .filter(|function| function.is_definition)
        .map(|function| function.id.symbol)
        .collect()
}

#[test]
fn an_emitted_slice_module_defines_the_slice_and_nothing_else() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    let out = scratch.path().join("slice.bc");
    let answer = query_json(
        &scratch,
        &catalog,
        &[
            "slice",
            "main",
            "add",
            "--emit-module",
            out.to_str().unwrap(),
        ],
    );

    let mut members: Vec<String> = answer["results"]["functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id["symbol"].as_str().unwrap().to_string())
        .collect();
    members.sort();
    assert_eq!(
        members,
        ["add", "add", "main"],
        "main, add's declaration beside it, and add's definition"
    );
    assert_eq!(answer["emitted"]["modules"], 2, "{answer}");
    assert_eq!(answer["emitted"]["functions"], 2, "{answer}");

    assert_eq!(
        defined_symbols(&out),
        BTreeSet::from(["add".to_string(), "main".to_string()]),
        "the slice's definitions, and neither `sub` nor `unused`"
    );
}

#[test]
fn an_empty_slice_emits_nothing_and_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "add", "main", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("no path from add to main; nothing to emit"),
        "{stderr}"
    );
    assert!(!out.exists(), "no empty module is written");
}

/// `a.c`'s static `helper` is on the path from `main` to `leaf`, and `leaf`
/// in `b.c` calls some other, external `helper` the program never defines.
#[test]
fn a_static_that_would_bind_another_module_s_call_is_not_emitted() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(
        &scratch,
        "statics",
        &[
            (
                "a.c",
                "int leaf(void);\nstatic int helper(void){return leaf();}\nint fa(void){return helper();}\n",
            ),
            (
                "b.c",
                "int helper(void);\nint leaf(void){return helper();}\n",
            ),
            ("main.c", "int fa(void);\nint main(void){return fa();}\n"),
        ],
    );
    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "main", "leaf", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a module was emitted: {stderr}");
    assert!(stderr.contains("helper` is static in module"), "{stderr}");
    assert!(!out.exists());
}

/// `a.c`'s static `note` is off the path from `main` to `leaf`, but `fa`, on
/// the path, still calls it; `b.c`'s external `note` is on the path. Cut
/// out, `fa`'s call would bind to `b.c`'s `note`.
#[test]
fn a_static_off_the_path_that_would_bind_to_another_definition_is_not_emitted() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(
        &scratch,
        "offpath",
        &[
            (
                "a.c",
                "int leaf(void);\nstatic int note(void){return 1;}\nint fa(void){note();return leaf();}\n",
            ),
            (
                "b.c",
                "int leaf(void){return 0;}\nint note(void){return leaf();}\n",
            ),
            (
                "main.c",
                "int fa(void);\nint note(void);\nint main(void){return fa()+note();}\n",
            ),
        ],
    );
    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "main", "leaf", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a module was emitted: {stderr}");
    assert!(stderr.contains("note` is static in module"), "{stderr}");
    assert!(!out.exists());
}

/// `a.c`'s static function `g` is on the path; `b.c`'s `leaf` reads a
/// global variable also named `g`. Cut out, the read would bind to the
/// function.
#[test]
fn a_static_function_named_like_another_module_s_variable_is_not_emitted() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(
        &scratch,
        "data",
        &[
            (
                "a.c",
                "int leaf(void);\nstatic int g(void){return leaf();}\nint fa(void){return g();}\n",
            ),
            ("b.c", "int g;\nint leaf(void){return g;}\n"),
            ("main.c", "int fa(void);\nint main(void){return fa();}\n"),
        ],
    );
    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "main", "leaf", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a module was emitted: {stderr}");
    assert!(
        stderr.contains(&format!("`{}` is static in module", spelled("g"))),
        "{stderr}"
    );
    assert!(!out.exists());
}

/// How the host target spells a C symbol in a symbol table: Mach-O adds a
/// leading `_`. The fixtures here compile for the host.
fn spelled(symbol: &str) -> String {
    if cfg!(target_vendor = "apple") {
        format!("_{symbol}")
    } else {
        symbol.to_string()
    }
}

/// A private function never appears in its own module's symbol listing, yet
/// cut out it becomes an external definition that `b`'s call to some other
/// `pf` would bind to.
#[test]
fn a_private_function_that_would_bind_another_module_s_call_is_not_emitted() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = ir_catalog(
        &scratch,
        &[
            (
                "a",
                "declare void @leaf()\n\
                 define private void @pf() {\n  call void @leaf()\n  ret void\n}\n\
                 define void @fa() {\n  call void @pf()\n  ret void\n}\n",
            ),
            (
                "b",
                "declare void @pf()\n\
                 define void @leaf() {\n  call void @pf()\n  ret void\n}\n",
            ),
            (
                "main",
                "declare void @fa()\n\
                 define void @main() {\n  call void @fa()\n  ret void\n}\n",
            ),
        ],
    );
    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "main", "leaf", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a module was emitted: {stderr}");
    assert!(stderr.contains("`pf` is static in module"), "{stderr}");
    assert!(!out.exists());
}

/// Both modules keep a `static usage`, but only off-path functions use it,
/// so neither copy reaches the emitted module and nothing collides.
#[test]
fn statics_that_stay_out_of_every_piece_do_not_block_the_module() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(
        &scratch,
        "usage",
        &[
            (
                "add.c",
                "static int usage(void){return 1;}\nint add(int a,int b){return a+b;}\nint sub(void){return usage();}\n",
            ),
            (
                "main.c",
                "static int usage(void){return 2;}\nint other(void){return usage();}\nint add(int a,int b);\nint main(void){return add(2,3);}\n",
            ),
        ],
    );
    let out = scratch.path().join("slice.bc");
    query_json(
        &scratch,
        &catalog,
        &[
            "slice",
            "main",
            "add",
            "--emit-module",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(
        defined_symbols(&out),
        BTreeSet::from(["add".to_string(), "main".to_string()])
    );
}

/// `llvm-extract` and `llvm-nm` come from the configured `llvm_bindir`, not
/// from beside `llvm-link`, which may live apart from the other LLVM tools.
#[cfg(unix)]
#[test]
fn a_slice_is_emitted_with_the_configured_llvm_bindir() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    let apart = scratch.path().join("llvm-link-apart");
    std::fs::create_dir(&apart).unwrap();
    std::os::unix::fs::symlink(llvm_bin("llvm-link"), apart.join("llvm-link")).unwrap();
    let config = scratch_rllvm_config(scratch.path());
    let contents: String = std::fs::read_to_string(&config)
        .unwrap()
        .lines()
        .map(|line| {
            if line.starts_with("llvm_link_filepath") {
                format!(
                    "llvm_link_filepath = '{}'\n",
                    apart.join("llvm-link").display()
                )
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    std::fs::write(&config, contents).unwrap();

    let out = scratch.path().join("slice.bc");
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", &config)
        .arg("--catalog")
        .arg(&catalog)
        .args(["--json", "slice", "main", "add", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.is_file(), "no module written");
}

/// A tool that runs and fails is reported with what it printed, never as a
/// module.
#[cfg(unix)]
#[test]
fn a_failing_link_is_an_execution_failure_with_its_stderr() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    let tools = scratch.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    for tool in ["llvm-extract", "llvm-nm"] {
        symlink(llvm_bin(tool), tools.join(tool)).unwrap();
    }
    let llvm_link = tools.join("llvm-link");
    std::fs::write(
        &llvm_link,
        "#!/bin/sh\necho 'refusing to link today' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&llvm_link, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = scratch.path().join("slice.bc");
    match emit_main_to_add(&catalog, &llvm_link, &tools, &out) {
        Err(rllvm_core::error::Error::ExecutionFailure(message)) => {
            assert!(message.contains("refusing to link today"), "{message}")
        }
        other => panic!("expected an execution failure, got {other:?}"),
    }
    assert!(!out.exists());
}

/// Emits `slice main add` through the library, linking with `llvm_link` and
/// taking the other tools from `llvm_bindir`.
fn emit_main_to_add(
    catalog: &Path,
    llvm_link: &Path,
    llvm_bindir: &Path,
    out: &Path,
) -> Result<rllvm_query::EmittedModule, rllvm_core::error::Error> {
    let session = rllvm_query::open(catalog).unwrap();
    let query = rllvm_query::Query::Slice {
        from: "main".into(),
        to: "add".into(),
        include_overlay: false,
        min_confidence: None,
    };
    let answer = rllvm_query::run(&session, &query).unwrap();
    let llvm_bindir = rllvm_core::config::LlvmBindir::configured(llvm_bindir);
    rllvm_query::emit_slice(&session, catalog, &answer, llvm_link, &llvm_bindir, out)
}

/// Every tool from the LLVM bindir that emitting needs is checked for, and a
/// missing one is named rather than run.
#[cfg(unix)]
#[test]
fn a_missing_tool_in_the_llvm_bindir_is_named() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    for (present, missing) in [("llvm-extract", "llvm-nm"), ("llvm-nm", "llvm-extract")] {
        let tools = scratch.path().join(format!("without-{missing}"));
        std::fs::create_dir(&tools).unwrap();
        std::os::unix::fs::symlink(llvm_bin(present), tools.join(present)).unwrap();
        let out = scratch.path().join("slice.bc");
        match emit_main_to_add(&catalog, &llvm_bin("llvm-link"), &tools, &out) {
            Err(rllvm_core::error::Error::MissingFile(message)) => {
                assert!(message.contains(missing), "{message}")
            }
            other => panic!("expected {missing} to be missing, got {other:?}"),
        }
        assert!(!out.exists());
    }
}

#[test]
fn the_text_answer_says_what_the_slice_module_holds() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "slice", SLICE_SOURCES);
    let out = scratch.path().join("slice.bc");
    // Plain whatever the caller's terminal asks for: a forced colour puts
    // escapes between the words looked for here.
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .arg("--catalog")
        .arg(&catalog)
        .args(["slice", "main", "add", "--emit-module"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("main -> add (call)\n"), "{text}");
    assert!(text.contains("add -> add (binding)\n"), "{text}");
    assert!(
        text.starts_with(&format!(
            "wrote {}: 2 functions from 2 modules\n",
            out.display()
        )),
        "{text}"
    );
}

#[test]
fn a_use_in_a_global_initializer_names_the_global() {
    // A dispatch table stores the address inside an aggregate constant, so
    // the function's user is the constant, not the global that holds it.
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(
        &scratch,
        "table",
        &[(
            "table.c",
            "struct command { int (*exec)(void); };\n\
             static int run(void) { return 0; }\n\
             struct command table[] = { { run } };\n\
             int (*direct)(void) = run;\n",
        )],
    );
    let value = query_json(&scratch, &catalog, &["uses", "run"]);
    let mut uses: Vec<(String, String)> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|use_fact| {
            (
                use_fact["kind"].as_str().unwrap().to_string(),
                use_fact["in_global"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    uses.sort();
    assert_eq!(
        uses,
        [
            ("global_initializer".to_string(), "direct".to_string()),
            ("global_initializer".to_string(), "table".to_string()),
        ],
        "{value}"
    );
}

/// Runs `rllvm-query --catalog <catalog> --json <args...>` and parses its
/// stdout. `--json` is explicit here, not the default: text is.
fn query_json(scratch: &tempfile::TempDir, catalog: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(catalog)
        .arg("--json")
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
    let modules: Vec<PathBuf> = ADD_AND_MAIN
        .iter()
        .map(|(name, source)| compile_bitcode(scratch, name, source))
        .collect();
    let first = modules[0].clone();
    let catalog = plain_module_catalog(scratch, &modules);
    (catalog, first)
}

/// Inventories each module file and writes one catalog holding them all.
fn plain_module_catalog(scratch: &tempfile::TempDir, modules: &[PathBuf]) -> PathBuf {
    let mut collected = Vec::new();
    let mut origin = None;
    for object in modules {
        let catalog =
            rllvm_core::catalog::inventory(object, scratch.path(), Some(&llvm_bin("llvm-dis")))
                .unwrap();
        origin.get_or_insert(catalog.origin.clone());
        collected.extend(catalog.modules);
    }
    let catalog = rllvm_core::catalog::ModuleCatalog::new(
        origin.expect("at least one module"),
        "recorded_modules",
        collected,
    );
    let path = scratch.path().join("plain-catalog.json");
    write_catalog_json(&path, &catalog);
    path
}

/// The shape every C++ program that uses templates or `inline` has: a
/// definition in a header, emitted into each translation unit that
/// instantiates it, and called from one that does not.
fn odr_template_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    std::fs::write(
        scratch.path().join("shared.h"),
        "template <typename T> T twice(T x) { return x + x; }\n",
    )
    .unwrap();
    archive_catalog_of(
        scratch,
        "odr",
        &[
            (
                "a.cpp",
                "#include \"shared.h\"\nint use_a(int x){ return twice(x); }\n",
            ),
            (
                "b.cpp",
                "#include \"shared.h\"\nint use_b(int x){ return twice(x); }\n",
            ),
            (
                "c.cpp",
                "#include \"shared.h\"\n\
                 extern template int twice<int>(int);\n\
                 int use_c(int x){ return twice(x); }\n",
            ),
        ],
    )
}

#[test]
fn a_template_instantiated_in_two_modules_resolves_to_one_definition() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = odr_template_catalog(&scratch);
    let answer = query_json(&scratch, &catalog, &["reach", "_Z5use_ci", TWICE_INT]);

    let path = answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("use_c calls twice, so reach must find a path: {answer}"));
    let binding = path
        .iter()
        .find(|step| step["kind"] == "binding")
        .unwrap_or_else(|| panic!("the path must cross a binding: {answer}"));
    assert_eq!(binding["symbol"], TWICE_INT);
    assert_eq!(
        binding["status"], "unique",
        "two linkonce_odr copies are one function, not two candidates"
    );
    assert!(
        answer["uncertainty"]["frontier"]
            .as_array()
            .unwrap()
            .is_empty(),
        "nothing about this program is ambiguous: {answer}"
    );
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
    let loaded = rllvm_query::load::LoadedModule {
        id: "future".into(),
        bytes: b"BC\xc0\xde\xff\xff\xff\xff".to_vec(),
        record: record_with_compiler_version("clang 99.0.0"),
    };
    let error = rllvm_query::extract::extract(&loaded, &Default::default()).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("99.0.0"),
        "must name the producer: {message}"
    );
    assert!(
        message.contains(&rllvm_query::llvm_version()),
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
    let loaded = rllvm_query::load::LoadedModule {
        id: "broken".into(),
        bytes: b"BC\xc0\xde\xff\xff\xff\xff".to_vec(),
        record: Default::default(),
    };
    let message = rllvm_query::extract::extract(&loaded, &Default::default())
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

/// Writes the inlining fixture and returns its bitcode: `add` is inlined into
/// `main`, so the call to `outer` has a leaf in `helper.h` and a frame in `t.c`.
fn inlined_fixture(scratch: &tempfile::TempDir) -> PathBuf {
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
    compile_bitcode_file(&scratch.path().join("t.c"), &["-g", "-O1"])
}

/// Every location in `facts`, inlined frames included.
fn all_locations(facts: &rllvm_query::ModuleFacts) -> Vec<&rllvm_query::SourceLocation> {
    fn walk<'a>(
        location: &'a rllvm_query::SourceLocation,
        into: &mut Vec<&'a rllvm_query::SourceLocation>,
    ) {
        into.push(location);
        for frame in &location.inlined_at {
            walk(frame, into);
        }
    }
    let mut into = Vec::new();
    let roots = facts
        .functions
        .iter()
        .filter_map(|f| f.location.as_ref())
        .chain(facts.call_sites.iter().filter_map(|c| c.location.as_ref()))
        .chain(facts.uses.iter().filter_map(|u| u.location.as_ref()));
    for location in roots {
        walk(location, &mut into);
    }
    into
}

/// Every module id `facts` names.
fn all_module_ids(facts: &rllvm_query::ModuleFacts) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for function in &facts.functions {
        ids.insert(function.id.module_id.clone());
    }
    for site in &facts.call_sites {
        ids.insert(site.id.function.module_id.clone());
        match &site.target {
            CallTarget::Direct { callee } => {
                ids.insert(callee.module_id.clone());
            }
            CallTarget::Indirect {
                llvm_target_bound: Some(bound),
                ..
            } => ids.extend(bound.iter().map(|id| id.module_id.clone())),
            _ => {}
        }
    }
    for use_fact in &facts.uses {
        ids.insert(use_fact.used.module_id.clone());
        ids.extend(use_fact.in_function.iter().map(|id| id.module_id.clone()));
    }
    ids
}

fn location_key(location: &rllvm_query::SourceLocation) -> PathBuf {
    location.directory.as_ref().map_or_else(
        || location.file.clone(),
        |directory| directory.join(&location.file),
    )
}

#[test]
fn neutral_facts_name_no_module_and_no_source_status() {
    let scratch = tempfile::tempdir().unwrap();
    let module = inlined_fixture(&scratch);
    let loaded = rllvm_query::load::LoadedModule {
        id: "t".into(),
        bytes: std::fs::read(&module).unwrap(),
        record: Default::default(),
    };
    let facts = rllvm_query::extract::extract_neutral(&loaded).unwrap();

    assert_eq!(all_module_ids(&facts), BTreeSet::from([String::new()]));
    let locations = all_locations(&facts);
    assert!(
        locations.iter().any(|l| !l.inlined_at.is_empty()),
        "fixture must inline"
    );
    for location in locations {
        assert_eq!(location.source_status, rllvm_query::SourceStatus::Unknown);
        assert_eq!(location.status_basis, None);
    }
}

#[test]
fn binding_stamps_the_module_on_every_id_and_status_on_every_frame() {
    use rllvm_core::catalog::DigestOrigin;
    use rllvm_query::{SourceStatus, load::SourceState};

    let scratch = tempfile::tempdir().unwrap();
    let module = inlined_fixture(&scratch);
    let loaded = rllvm_query::load::LoadedModule {
        id: "t".into(),
        bytes: std::fs::read(&module).unwrap(),
        record: Default::default(),
    };
    let mut facts = rllvm_query::extract::extract_neutral(&loaded).unwrap();

    let call = facts
        .call_sites
        .iter()
        .find(|c| matches!(&c.target, CallTarget::Direct { callee } if callee.symbol == "outer"))
        .expect("call to outer");
    let leaf = call
        .location
        .clone()
        .expect("an inlined call has a location");
    let frame = leaf.inlined_at[0].clone();
    let status = HashMap::from([
        (
            ("m".to_string(), location_key(&leaf)),
            SourceState {
                status: SourceStatus::Current,
                basis: Some(DigestOrigin::Capture),
            },
        ),
        (
            ("m".to_string(), location_key(&frame)),
            SourceState {
                status: SourceStatus::Modified,
                basis: Some(DigestOrigin::Compiler),
            },
        ),
    ]);

    facts.bind_to_catalog("m", &status);

    assert_eq!(all_module_ids(&facts), BTreeSet::from(["m".to_string()]));
    let call = facts
        .call_sites
        .iter()
        .find(|c| matches!(&c.target, CallTarget::Direct { callee } if callee.symbol == "outer"))
        .unwrap();
    let leaf = call.location.as_ref().unwrap();
    assert_eq!(leaf.source_status, SourceStatus::Current);
    assert_eq!(leaf.status_basis, Some(DigestOrigin::Capture));
    assert_eq!(leaf.inlined_at[0].source_status, SourceStatus::Modified);
    assert_eq!(
        leaf.inlined_at[0].status_basis,
        Some(DigestOrigin::Compiler)
    );
}

#[test]
fn an_inlined_frame_carries_its_own_file_not_the_leaf_s() {
    let scratch = tempfile::tempdir().unwrap();
    // `add` is inlined into `main`, so the call to `outer` sits in `main`
    // with a leaf location in the header and a frame above it in `t.c`. Each
    // frame resolves through its own scope; copying the leaf's file upward
    // would report the header twice.
    let module = inlined_fixture(&scratch);
    let facts = extract_module(&module);

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

/// A function pointer passed as an argument to a `noinline` callee. At `-O0`
/// it lands in a stack slot CVP cannot follow; at `-O1` it stays in SSA.
/// One source, two optimization levels: the difference under test is the
/// flags, so the program must not differ too.
const NOINLINE_ARGUMENT: &str = "static int add(int a,int b){return a+b;}\n\
     static int sub(int a,int b){return a-b;}\n\
     __attribute__((noinline)) static int apply(int(*f)(int,int),int x){ return f(x,3); }\n\
     int run(int x){ return apply(add,x) + apply(sub,x); }\n";

/// A function pointer in a local struct field. At `-O0` the field stays in an
/// alloca; at `-O1` SROA promotes it to SSA. Again one source, two levels.
const STRUCT_FIELD: &str = "static int add(int a,int b){return a+b;}\n\
     static int sub(int a,int b){return a-b;}\n\
     struct ops { int (*op)(int,int); };\n\
     int run(int x){ struct ops o; o.op = x ? add : sub; return o.op(1,2); }\n";

/// The bound's symbols, sorted, so a test pins the set rather than whatever
/// order CVP happened to list its metadata operands in.
fn bound_symbols(facts: &rllvm_query::ModuleFacts, expectation: &str) -> Vec<String> {
    let mut names: Vec<String> = indirect_bound(facts)
        .unwrap_or_else(|| panic!("{expectation}"))
        .iter()
        .map(|function| function.symbol.clone())
        .collect();
    names.sort();
    names
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
    assert_eq!(
        bound_symbols(&facts, "internal global is eligible"),
        ["add", "sub"]
    );
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
    let facts = extract_source(&scratch, NOINLINE_ARGUMENT);
    assert_unresolved_indirect_site(&facts, "-O0 routes the argument through a stack slot");
}

#[test]
fn cvp_bounds_the_same_argument_at_o1() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(&scratch, NOINLINE_ARGUMENT, &["-g", "-O1"]);
    assert_eq!(indirect_bound(&facts).map(|bound| bound.len()), Some(2));
}

/// From the spec's verified-constraints table: a struct field, the fourth
/// eligibility row alongside the internal global, the external global, and
/// the `noinline` argument above.
#[test]
fn cvp_does_not_bound_a_struct_field_at_o0() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(&scratch, STRUCT_FIELD);
    assert_unresolved_indirect_site(
        &facts,
        "-O0 leaves the field in an alloca CVP cannot follow",
    );
}

#[test]
fn cvp_bounds_the_same_struct_field_at_o1() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(&scratch, STRUCT_FIELD, &["-g", "-O1"]);
    assert_eq!(
        bound_symbols(&facts, "SROA promotes the field to SSA at -O1"),
        ["add", "sub"]
    );
}

#[test]
fn cvp_bounds_a_four_candidate_set() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source(&scratch, &switch_dispatch_source(4));
    assert_eq!(
        bound_symbols(&facts, "four candidates is within CVP's default limit"),
        ["f1", "f2", "f3", "f4"]
    );
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
fn indirect_target(facts: &rllvm_query::ModuleFacts) -> &CallTarget {
    facts
        .call_sites
        .iter()
        .map(|site| &site.target)
        .find(|target| matches!(target, CallTarget::Indirect { .. }))
        .expect("fixture must contain an indirect call site")
}

fn indirect_bound(facts: &rllvm_query::ModuleFacts) -> Option<Vec<rllvm_query::FunctionId>> {
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
fn assert_unresolved_indirect_site(facts: &rllvm_query::ModuleFacts, message: &str) {
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

/// Function pointers reached through record fields, in every shape the
/// field evidence distinguishes: a plain field, a nested one, an anonymous
/// typedef'd record, initializers of named and literal IR type, and a plain
/// function-pointer variable that names no field at all.
const FIELD_SOURCE: &str = r#"
typedef void (*cb_t)(void *, long);
struct ops { int tag; cb_t on_event; cb_t on_close; };
typedef struct { int tag; cb_t on_event; } anon_ops;
struct inner { cb_t a; cb_t b; };
struct outer { int x; struct inner in; };
static void h1(void *p, long n) {}
static void h2(void *p, long n) {}
static void h3(void *p, long n) {}
const struct ops table = { 1, h1, h2 };
struct outer nested = { 1, { h1, h2 } };
void init(struct ops *o) { o->on_event = h3; }
void dispatch(struct ops *o, void *p) { o->on_event(p, 4); }
void set_anon(anon_ops *o) { o->on_event = h1; }
void call_anon(anon_ops *o) { o->on_event(0, 1); }
void set_inner(struct outer *o) { o->in.b = h1; }
void call_inner(struct outer *o) { o->in.b(0, 1); }
void call_plain(cb_t f) { f(0, 1); }
"#;

fn field(record: &str, offset: u64) -> rllvm_query::FieldRef {
    rllvm_query::FieldRef {
        record: record.into(),
        offset,
    }
}

/// The field evidence on the one indirect call site in `function`. Panics
/// unless there is exactly one, so a `None` expectation cannot pass because
/// the site vanished.
fn site_field(
    facts: &rllvm_query::ModuleFacts,
    function: &str,
) -> Option<rllvm_query::FieldEvidence> {
    let fields: Vec<_> = facts
        .call_sites
        .iter()
        .filter(|site| site.id.function.symbol == function)
        .filter_map(|site| match &site.target {
            CallTarget::Indirect { via_field, .. } => Some(via_field.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(fields.len(), 1, "{function} must hold one indirect site");
    fields.into_iter().next().unwrap()
}

/// The field evidence on every use of `used` held by `holder`, a function
/// or a global, of `kind`. Panics when there is none.
fn use_fields(
    facts: &rllvm_query::ModuleFacts,
    used: &str,
    holder: &str,
    kind: rllvm_query::UseKind,
) -> Vec<Option<rllvm_query::FieldEvidence>> {
    let fields: Vec<_> = facts
        .uses
        .iter()
        .filter(|use_fact| use_fact.used.symbol == used && use_fact.kind == kind)
        .filter(|use_fact| {
            use_fact.in_global.as_deref() == Some(holder)
                || use_fact
                    .in_function
                    .as_ref()
                    .is_some_and(|function| function.symbol == holder)
        })
        .map(|use_fact| use_fact.field.clone())
        .collect();
    assert!(!fields.is_empty(), "no {kind:?} use of {used} in {holder}");
    fields
}

/// The one field a use names, with the evidence it came from.
fn one_use_field(
    facts: &rllvm_query::ModuleFacts,
    used: &str,
    holder: &str,
    kind: rllvm_query::UseKind,
) -> rllvm_query::FieldEvidence {
    match use_fields(facts, used, holder, kind).as_slice() {
        [Some(evidence)] => evidence.clone(),
        other => panic!("{used} in {holder}: expected one field, got {other:?}"),
    }
}

/// -O0 addresses the field with a typed GEP; -O2 erases the type and leaves
/// a byte offset with a TBAA tag. Both must name the same field, or a store
/// compiled at one level never meets a call compiled at the other.
#[test]
fn a_field_dispatch_names_the_same_field_at_every_optimization_level() {
    use rllvm_query::FieldBasis;
    for (level, basis) in [("-O0", FieldBasis::StructGep), ("-O2", FieldBasis::Tbaa)] {
        let scratch = tempfile::tempdir().unwrap();
        let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &[level, "-g"]);
        let evidence = site_field(&facts, "dispatch").unwrap_or_else(|| panic!("{level}"));
        assert_eq!(evidence.field, field("ops", 8), "{level}");
        assert_eq!(evidence.basis, basis, "{level}");
        assert_eq!(evidence.name.as_deref(), Some("on_event"), "{level}");
    }
}

#[test]
fn a_store_and_an_initializer_name_the_field_the_call_reads() {
    use rllvm_query::{FieldBasis, UseKind};
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &["-O0", "-g"]);

    let stored = one_use_field(&facts, "h3", "init", UseKind::StoredToMemory);
    assert_eq!(stored.field, field("ops", 8));
    assert_eq!(stored.basis, FieldBasis::StructGep);

    // `table` has literal IR type for its padding, so its record comes from
    // the global's debug-info type.
    let first = one_use_field(&facts, "h1", "table", UseKind::GlobalInitializer);
    assert_eq!(first.field, field("ops", 8));
    assert_eq!(first.basis, FieldBasis::DebugInfo);
    assert_eq!(first.name.as_deref(), Some("on_event"));
    let second = one_use_field(&facts, "h2", "table", UseKind::GlobalInitializer);
    assert_eq!(second.field, field("ops", 16));
    assert_eq!(second.basis, FieldBasis::DebugInfo);
    assert_eq!(second.name.as_deref(), Some("on_close"));

    // `nested` is literal outside, but its inner record is a named IR struct.
    for (used, offset, name) in [("h1", 0, "a"), ("h2", 8, "b")] {
        let evidence = one_use_field(&facts, used, "nested", UseKind::GlobalInitializer);
        assert_eq!(evidence.field, field("inner", offset), "{used}");
        assert_eq!(evidence.basis, FieldBasis::Initializer, "{used}");
        assert_eq!(evidence.name.as_deref(), Some(name), "{used}");
    }
}

#[test]
fn a_nested_field_names_its_innermost_record() {
    use rllvm_query::UseKind;
    for level in ["-O0", "-O2"] {
        let scratch = tempfile::tempdir().unwrap();
        let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &[level]);
        let site = site_field(&facts, "call_inner").unwrap_or_else(|| panic!("{level}"));
        assert_eq!(site.field, field("inner", 8), "{level}");
        let stored = one_use_field(&facts, "h1", "set_inner", UseKind::StoredToMemory);
        assert_eq!(stored.field, field("inner", 8), "{level}");
    }
}

/// Clang names the IR type of a typedef'd anonymous struct after the
/// typedef, but its TBAA type node is unnamed. The -O2 site therefore names
/// no field, rather than one borrowed from debug info.
#[test]
fn an_anonymous_record_is_named_by_its_typedef_only_where_the_ir_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &["-O0", "-g"]);
    let evidence = site_field(&facts, "call_anon").expect("-O0 names the typed GEP");
    assert_eq!(evidence.field, field("anon_ops", 8));

    let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &["-O2", "-g"]);
    assert_eq!(site_field(&facts, "call_anon"), None);
}

#[test]
fn a_plain_function_pointer_names_no_field() {
    for level in ["-O0", "-O2"] {
        let scratch = tempfile::tempdir().unwrap();
        let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &[level, "-g"]);
        assert_eq!(site_field(&facts, "call_plain"), None, "{level}");
    }
}

#[test]
fn an_initializer_without_debug_info_names_no_literal_field() {
    use rllvm_query::UseKind;
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_source_with_flags(&scratch, FIELD_SOURCE, &["-O0"]);
    for used in ["h1", "h2"] {
        assert_eq!(
            use_fields(&facts, used, "table", UseKind::GlobalInitializer),
            [None],
            "{used}: a literal type names no record without debug info"
        );
    }
    let evidence = one_use_field(&facts, "h2", "nested", UseKind::GlobalInitializer);
    assert_eq!(evidence.field, field("inner", 8));
    assert_eq!(evidence.name, None, "member names come from debug info");
}

/// C++ spells the record `%"struct.n::S"` in IR and `_ZTSN1n1SE` in TBAA;
/// both must read as `n::S`. The member name comes from the debug-info
/// record found by that same identifier.
#[test]
fn a_cxx_record_reads_the_same_through_gep_and_tbaa() {
    use rllvm_query::FieldBasis;
    let source = "namespace n { struct S { void (*f)(int); }; }\n\
                  void c(n::S* s){ s->f(1); }\n";
    for (level, basis) in [("-O0", FieldBasis::StructGep), ("-O2", FieldBasis::Tbaa)] {
        let scratch = tempfile::tempdir().unwrap();
        let facts = extract_named(&scratch, "t.cpp", source, &[level, "-g"]);
        let evidence = site_field(&facts, "_Z1cPN1n1SE").unwrap_or_else(|| panic!("{level}"));
        assert_eq!(evidence.field, field("n::S", 0), "{level}");
        assert_eq!(evidence.basis, basis, "{level}");
        assert_eq!(evidence.name.as_deref(), Some("f"), "{level}");
    }
}

/// A C++ global of a record in an anonymous namespace, given the literal IR
/// type padding produces. Debug info names the record plain `S` with no
/// identifier, while IR and TBAA call it `(anonymous namespace)::S`; taking
/// `S` would join it with an unrelated `::S`. The scope `{SCOPE}` is
/// substituted: the namespace, or the file as a C record would have.
const ANONYMOUS_NAMESPACE_TABLE: &str = r#"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx26.0.0"

@_ZL5table = internal constant { i32, [4 x i8], ptr } { i32 1, [4 x i8] zeroinitializer, ptr @_ZL1hi }, align 8, !dbg !0
@llvm.used = appending global [1 x ptr] [ptr @_ZL5table], section "llvm.metadata"

define internal void @_ZL1hi(i32 %0) {
  ret void
}

!llvm.module.flags = !{!15}
!llvm.dbg.cu = !{!2}

!0 = !DIGlobalVariableExpression(var: !1, expr: !DIExpression())
!1 = distinct !DIGlobalVariable(name: "table", linkageName: "_ZL5table", scope: !2, file: !3, line: 3, type: !5, isLocal: true, isDefinition: true)
!2 = distinct !DICompileUnit(language: DW_LANG_C_plus_plus_14, file: !3, producer: "clang", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug, globals: !4)
!3 = !DIFile(filename: "anon.cpp", directory: "/tmp")
!4 = !{!0}
!5 = !DIDerivedType(tag: DW_TAG_const_type, baseType: !6)
!6 = distinct !DICompositeType(tag: DW_TAG_structure_type, name: "S", scope: {SCOPE}, file: !3, line: 2, size: 128, flags: DIFlagTypePassByValue, elements: !8)
!7 = !DINamespace(scope: null)
!8 = !{!9, !11}
!9 = !DIDerivedType(tag: DW_TAG_member, name: "tag", scope: !6, file: !3, line: 2, baseType: !10, size: 32)
!10 = !DIBasicType(name: "int", size: 32, encoding: DW_ATE_signed)
!11 = !DIDerivedType(tag: DW_TAG_member, name: "f", scope: !6, file: !3, line: 2, baseType: !12, size: 64, offset: 64)
!12 = !DIDerivedType(tag: DW_TAG_pointer_type, baseType: !13, size: 64)
!13 = !DISubroutineType(types: !14)
!14 = !{null, !10}
!15 = !{i32 2, !"Debug Info Version", i32 3}
"#;

#[test]
fn a_scoped_cxx_record_without_an_identifier_names_no_literal_field() {
    use rllvm_query::UseKind;
    let table_field = |scope: &str| {
        let scratch = tempfile::tempdir().unwrap();
        let ir = ANONYMOUS_NAMESPACE_TABLE.replace("{SCOPE}", scope);
        let facts = extract_module(&assemble_ir(&scratch, &ir));
        use_fields(&facts, "_ZL1hi", "_ZL5table", UseKind::GlobalInitializer)
    };
    assert_eq!(
        table_field("!7"),
        [None],
        "a namespaced record's plain name is not the IR's"
    );
    // The same node at file scope, as C writes it, does name the field: the
    // refusal above is the scope's doing, not a failed read.
    let file_scope = table_field("!3");
    let [Some(evidence)] = file_scope.as_slice() else {
        panic!("a file-scope record names its field: {file_scope:?}");
    };
    assert_eq!(evidence.field, field("S", 8));
    assert_eq!(evidence.basis, rllvm_query::FieldBasis::DebugInfo);
}

/// A source clang compiles into one named function, and the category that
/// function's linkage must arrive as.
struct LinkageCase {
    file: &'static str,
    source: &'static str,
    flags: &'static [&'static str],
    symbol: &'static str,
    linkage: Linkage,
}

/// One case per linkage kind a C or C++ compile can produce. Both ODR kinds
/// are here because they reach `bind` as one category while meaning slightly
/// different things to the linker, and `weak` is here beside them because it
/// looks identical in the IR yet promises nothing about the copies.
const LINKAGE_CASES: &[LinkageCase] = &[
    LinkageCase {
        file: "external.c",
        source: "int f(int x){return x;}\n",
        flags: &["-g", "-O0"],
        symbol: "f",
        linkage: Linkage::External,
    },
    LinkageCase {
        file: "internal.c",
        source: "static int s(int x){return x;}\nint (*take(void))(int){return s;}\n",
        flags: &["-g", "-O0"],
        symbol: "s",
        linkage: Linkage::Internal,
    },
    // An implicit instantiation: every translation unit that uses the
    // template emits its own copy as `linkonce_odr`.
    LinkageCase {
        file: "implicit.cpp",
        source: "template <typename T> T twice(T x){return x+x;}\nint use(int x){return twice(x);}\n",
        flags: &["-g", "-O0"],
        symbol: TWICE_INT,
        linkage: Linkage::Odr,
    },
    // An explicit instantiation definition: `weak_odr`, which differs from
    // `linkonce_odr` only in that the linker may not discard it when unused.
    LinkageCase {
        file: "explicit.cpp",
        source: "template <typename T> T twice(T x){return x+x;}\ntemplate int twice<int>(int);\n",
        flags: &["-g", "-O0"],
        symbol: TWICE_INT,
        linkage: Linkage::Odr,
    },
    LinkageCase {
        file: "weak.c",
        source: "__attribute__((weak)) int pick(void){return 1;}\n",
        flags: &["-g", "-O0"],
        symbol: "pick",
        linkage: Linkage::Weak,
    },
    // A body kept only so callers can inline it. Nothing emits the symbol,
    // so it cannot satisfy another module's declaration.
    LinkageCase {
        file: "available.c",
        source: "__attribute__((always_inline)) inline int ei(int x){return x+1;}\n\
                 int (*take(void))(int){return ei;}\n",
        flags: &["-g", "-O0", "-std=c99"],
        symbol: "ei",
        linkage: Linkage::AvailableExternally,
    },
];

/// `int twice<int>(int)`, the instantiation both C++ linkage cases emit.
const TWICE_INT: &str = "_Z5twiceIiET_S0_";

#[test]
fn every_linkage_kind_clang_emits_reaches_the_facts_as_its_own_category() {
    let scratch = tempfile::tempdir().unwrap();
    for case in LINKAGE_CASES {
        let facts = extract_named(&scratch, case.file, case.source, case.flags);
        let function = facts
            .functions
            .iter()
            .find(|function| function.id.symbol == case.symbol)
            .unwrap_or_else(|| panic!("{} must define {}", case.file, case.symbol));
        assert_eq!(
            function.linkage, case.linkage,
            "{} defines {} with the wrong category",
            case.file, case.symbol
        );
    }
}

fn extract_source(scratch: &tempfile::TempDir, source: &str) -> rllvm_query::ModuleFacts {
    extract_source_with_flags(scratch, source, &["-g", "-O0"])
}

fn extract_source_with_flags(
    scratch: &tempfile::TempDir,
    source: &str,
    flags: &[&str],
) -> rllvm_query::ModuleFacts {
    extract_named(scratch, "t.c", source, flags)
}

/// Writes `name` and extracts from its bitcode. The extension decides the
/// language: clang's driver compiles a `.cpp` fixture as C++.
fn extract_named(
    scratch: &tempfile::TempDir,
    name: &str,
    source: &str,
    flags: &[&str],
) -> rllvm_query::ModuleFacts {
    std::fs::write(scratch.path().join(name), source).unwrap();
    compile_and_extract(scratch, name, flags)
}

/// Compiles a source already written into `scratch`, so a fixture can put a
/// header beside it first.
fn compile_and_extract(
    scratch: &tempfile::TempDir,
    name: &str,
    flags: &[&str],
) -> rllvm_query::ModuleFacts {
    extract_module(&compile_bitcode_file(&scratch.path().join(name), flags))
}

/// Extracts one module file under the fixed id `t`.
fn extract_module(module: &Path) -> rllvm_query::ModuleFacts {
    let loaded = rllvm_query::load::LoadedModule {
        id: "t".into(),
        bytes: std::fs::read(module).unwrap(),
        record: Default::default(),
    };
    rllvm_query::extract::extract(&loaded, &Default::default()).unwrap()
}

/// Assembles textual IR to a `.bc` file, for the shapes a compiler will not
/// produce on request: a module merged from two languages, or one naming no
/// producer.
fn assemble_ir(scratch: &tempfile::TempDir, ir: &str) -> PathBuf {
    let source = scratch.path().join("t.ll");
    let module = scratch.path().join("t.bc");
    std::fs::write(&source, ir).unwrap();
    let status = Command::new(llvm_bin("llvm-as"))
        .arg(&source)
        .arg("-o")
        .arg(&module)
        .status()
        .unwrap();
    assert!(status.success(), "llvm-as rejected the fixture");
    module
}

fn extract_ir(scratch: &tempfile::TempDir, ir: &str) -> rllvm_query::ModuleFacts {
    extract_module(&assemble_ir(scratch, ir))
}

fn language_of(facts: &rllvm_query::ModuleFacts, symbol: &str) -> Option<SourceLanguage> {
    facts
        .functions
        .iter()
        .find(|function| function.id.symbol == symbol)
        .unwrap_or_else(|| panic!("no function {symbol}"))
        .language
}

/// What `llvm-link` makes of a C module and a Rust one built with `-g`:
/// both producers in `llvm.ident`, and one compile unit per original file.
const MERGED_IR: &str = r#"define void @rust_fn() !dbg !10 {
  ret void
}

define void @c_fn() !dbg !20 {
  ret void
}

define void @no_debug() {
  ret void
}

!llvm.dbg.cu = !{!1, !2}
!llvm.ident = !{!3, !4}
!llvm.module.flags = !{!5}

!1 = distinct !DICompileUnit(language: DW_LANG_Rust, file: !6, producer: "rustc", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)
!2 = distinct !DICompileUnit(language: DW_LANG_C11, file: !7, producer: "clang", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)
!3 = !{!"rustc version 1.98.0 (88d9e12ae 2026-08-18)"}
!4 = !{!"clang version 23.1.1"}
!5 = !{i32 2, !"Debug Info Version", i32 3}
!6 = !DIFile(filename: "lib.rs", directory: "/src")
!7 = !DIFile(filename: "c.c", directory: "/src")
!8 = !DISubroutineType(types: !9)
!9 = !{}
!10 = distinct !DISubprogram(name: "rust_fn", scope: !6, file: !6, line: 1, type: !8, spFlags: DISPFlagDefinition, unit: !1)
!20 = distinct !DISubprogram(name: "c_fn", scope: !7, file: !7, line: 1, type: !8, spFlags: DISPFlagDefinition, unit: !2)
"#;

/// rustc's module without `-g`: the producer is the only evidence. Also
/// declares `imported`, an external symbol this module borrows rather than
/// writes -- the producer must not speak for it.
const RUSTC_IR_WITHOUT_DEBUG_INFO: &str = r#"define void @exported() {
  ret void
}

declare void @imported()

!llvm.ident = !{!0}
!0 = !{!"rustc version 1.98.0 (88d9e12ae 2026-08-18)"}
"#;

const IR_WITHOUT_PRODUCER: &str = "define void @x() {\n  ret void\n}\n";

fn by_debug_info(name: Language) -> Option<SourceLanguage> {
    Some(SourceLanguage {
        name,
        basis: LanguageBasis::DebugInfo,
    })
}

fn by_producer(name: Language) -> Option<SourceLanguage> {
    Some(SourceLanguage {
        name,
        basis: LanguageBasis::Producer,
    })
}

#[test]
fn debug_info_attributes_each_function_of_a_merged_module() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_ir(&scratch, MERGED_IR);
    assert_eq!(
        language_of(&facts, "rust_fn"),
        by_debug_info(Language::Rust)
    );
    assert_eq!(language_of(&facts, "c_fn"), by_debug_info(Language::Other));
    assert_eq!(
        language_of(&facts, "no_debug"),
        None,
        "mixed producers cannot speak for a function without debug info"
    );
    assert_eq!(
        facts.producers,
        [
            "rustc version 1.98.0 (88d9e12ae 2026-08-18)",
            "clang version 23.1.1"
        ]
    );
}

#[test]
fn a_dwarf_6_language_name_attributes_too() {
    let scratch = tempfile::tempdir().unwrap();
    let ir = MERGED_IR.replace(
        "language: DW_LANG_Rust",
        "sourceLanguageName: DW_LNAME_Rust",
    );
    let facts = extract_ir(&scratch, &ir);
    assert_eq!(
        language_of(&facts, "rust_fn"),
        by_debug_info(Language::Rust)
    );
}

#[test]
fn the_producer_attributes_a_module_without_debug_info() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_ir(&scratch, RUSTC_IR_WITHOUT_DEBUG_INFO);
    assert_eq!(language_of(&facts, "exported"), by_producer(Language::Rust));
    assert_eq!(
        language_of(&facts, "imported"),
        None,
        "a declaration is not written in the module that declares it"
    );
}

#[test]
fn a_module_naming_no_producer_is_unattributed() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = extract_ir(&scratch, IR_WITHOUT_PRODUCER);
    assert_eq!(language_of(&facts, "x"), None);
    assert!(facts.producers.is_empty());
}

#[test]
fn a_clang_module_is_attributed_to_another_language() {
    let scratch = tempfile::tempdir().unwrap();
    let source = "int f(void) { return 1; }\n";
    let without_debug_info = extract_source_with_flags(&scratch, source, &["-O0"]);
    assert_eq!(
        language_of(&without_debug_info, "f"),
        by_producer(Language::Other)
    );
    let debug = extract_source(&scratch, source);
    assert_eq!(language_of(&debug, "f"), by_debug_info(Language::Other));
}

#[test]
fn an_answer_quotes_each_module_s_producers() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog, _) = write_catalog_with_one_module(&scratch);
    let answer = query_json(&scratch, &catalog, &["externals"]);
    let producers = answer["analysis"]["modules"][0]["producers"]
        .as_array()
        .expect("an analyzed clang module names its producer");
    assert!(
        producers[0].as_str().unwrap().contains("clang version"),
        "got {producers:?}"
    );
}

/// Stands in for a catalog whose capture recorded a compiler this LLVM is
/// older than.
fn record_with_compiler_version(version: &str) -> rllvm_core::catalog::ModuleRecord {
    rllvm_core::catalog::ModuleRecord {
        compiler: Some(rllvm_core::catalog::CompilerIdentity {
            path: PathBuf::from("clang"),
            realpath: None,
            version: version.to_string(),
            sha256: None,
        }),
        ..Default::default()
    }
}

/// The template from the #184 repro: one definition, two instantiations, each
/// emitted `linkonce_odr` under its own mangled name.
const TWICE_CXX: &str = "template <typename T> T twice(T x) { return x + x; }\n\
     int main() { return twice<int>(2) + (int)twice<double>(1.5); }\n";

fn cxx_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    let module = compile_bitcode(scratch, "twice.cpp", TWICE_CXX);
    let mut catalog =
        rllvm_core::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis")))
            .unwrap();
    catalog.modules[0].id = "twice".to_string();
    let path = scratch.path().join("catalog.json");
    write_catalog_json(&path, &catalog);
    path
}

#[test]
fn a_cxx_answer_carries_the_reading_of_the_symbols_it_prints() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = cxx_catalog(&scratch);

    let answer = query_json(&scratch, &catalog, &["defs", "_Z5twiceIiET_S0_"]);
    assert_eq!(
        answer["results"][0]["function"]["symbol"], "_Z5twiceIiET_S0_",
        "the mangled name stays the identity"
    );
    assert_eq!(
        answer["symbols"]["_Z5twiceIiET_S0_"], "int twice<int>(int)",
        "and the table carries its reading: {answer}"
    );
    assert_eq!(answer["resolution"][0]["matched"], "mangled");
}

#[test]
fn a_demangled_name_is_accepted_as_input() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = cxx_catalog(&scratch);

    let answer = query_json(&scratch, &catalog, &["defs", "int twice<int>(int)"]);
    assert_eq!(answer["resolution"][0]["matched"], "demangled");
    assert_eq!(
        answer["results"][0]["function"]["symbol"],
        "_Z5twiceIiET_S0_"
    );
    assert_eq!(
        answer["results"].as_array().unwrap().len(),
        1,
        "the full reading names one instantiation, not both"
    );
}

/// The convenience the issue asked for, and the honesty it costs: `twice`
/// finds both instantiations, and the answer says it got there fuzzily.
#[test]
fn a_bare_identifier_finds_every_instantiation_and_says_it_was_fuzzy() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = cxx_catalog(&scratch);

    let answer = query_json(&scratch, &catalog, &["defs", "twice"]);
    assert_eq!(answer["resolution"][0]["matched"], "fuzzy");
    assert_eq!(
        answer["resolution"][0]["symbols"],
        serde_json::json!(["_Z5twiceIdET_S0_", "_Z5twiceIiET_S0_"])
    );
    assert_eq!(answer["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        answer["symbols"]["_Z5twiceIdET_S0_"],
        "double twice<double>(double)"
    );
}

/// A C program's answers gain neither block's noise: `main` is its own name.
#[test]
fn a_c_answer_carries_no_symbol_table() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);

    let answer = query_json(&scratch, &catalog, &["callers", "add"]);
    assert_eq!(answer["resolution"][0]["matched"], "mangled");
    assert!(
        answer.get("symbols").is_none(),
        "nothing in a C answer demangles: {answer}"
    );
}

const RUST_EXPORTS: &str = r#"#[unsafe(no_mangle)]
pub extern "C" fn lib_add(a: i32, b: i32) -> i32 { helper(a) + b }

#[unsafe(export_name = "renamed")]
pub extern "C" fn lib_renamed() {}

pub extern "C" fn stays_mangled() {}

#[inline(never)]
fn helper(a: i32) -> i32 { a * 2 }
"#;

/// Real rustc output rather than a hand-written `llvm.ident`, without `-g`,
/// beside a C module: the attribution rests on what rustc actually writes.
#[test]
fn ffi_exports_lists_what_no_mangle_and_export_name_make_c_callable() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("lib.rs");
    let rust = scratch.path().join("lib.bc");
    std::fs::write(&source, RUST_EXPORTS).unwrap();
    let status = Command::new("rustc")
        .args([
            "--crate-type=staticlib",
            "--emit=llvm-bc",
            "-C",
            "codegen-units=1",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&rust)
        .status()
        .unwrap();
    assert!(status.success(), "rustc failed");
    let c = compile_bitcode(&scratch, "c.c", "int c_fn(void) { return 1; }\n");

    let catalog = plain_module_catalog(&scratch, &[rust, c]);
    let answer = query_json(&scratch, &catalog, &["ffi-exports"]);
    assert_eq!(
        answer["analysis"]["analyzed"], 2,
        "a module failed to parse -- is rustc's LLVM newer than the reader?: {answer}"
    );
    let symbols: Vec<&str> = answer["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["function"]["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(symbols, ["lib_add", "renamed"]);
    assert_eq!(answer["uncertainty"]["functions_of_unknown_language"], 0);
}

/// `MERGED_IR` holds `no_debug`, a function with neither debug info nor a
/// single producer to fall back on: it is counted as unknown, not searched,
/// and so never appears among the results.
#[test]
fn a_merged_module_without_debug_info_is_counted_not_searched() {
    let scratch = tempfile::tempdir().unwrap();
    let module = assemble_ir(&scratch, MERGED_IR);
    let catalog = plain_module_catalog(&scratch, &[module]);

    let answer = query_json(&scratch, &catalog, &["ffi-exports"]);
    assert_eq!(
        answer["analysis"]["analyzed"], 1,
        "the merged fixture failed to parse: {answer}"
    );
    let symbols: Vec<&str> = answer["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["function"]["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(symbols, ["rust_fn"]);
    assert_eq!(answer["uncertainty"]["functions_of_unknown_language"], 1);
}

// --- MCP stdio server -------------------------------------------------
//
// Nested in its own module so `cargo test -p rllvm-query --test query mcp`
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
    /// `rllvm_query::open` to produce a `Session` the server can answer over.
    /// The MCP tests below only need a session to exist, not any particular
    /// program in it.
    fn empty_catalog(scratch: &tempfile::TempDir) -> PathBuf {
        let catalog = rllvm_core::catalog::ModuleCatalog::new(
            rllvm_core::catalog::CatalogOrigin {
                kind: "test".into(),
                input: PathBuf::from("test"),
                sha256: None,
            },
            "test",
            vec![],
        );
        let catalog_path = scratch.path().join("catalog.json");
        rllvm_core::catalog::write_catalog(&catalog_path, &catalog).unwrap();
        catalog_path
    }

    /// Spawns `rllvm-query [--catalog <catalog>] mcp`, writes each request on
    /// its own line, closes stdin so the server sees EOF and exits, and
    /// returns everything the process wrote to stdout, raw.
    ///
    /// Several requests share one process, which is the only way to exercise
    /// a session that loads a catalog and then queries what it loaded.
    fn mcp_raw_session(
        scratch: &tempfile::TempDir,
        catalog: Option<&Path>,
        requests: &[&str],
    ) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rllvm-query"));
        command.env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()));
        if let Some(catalog) = catalog {
            command.arg("--catalog").arg(catalog);
        }
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            // The transport is one JSON-RPC frame per line. A fixture may
            // format its JSON across multiple source lines for readability
            // (e.g. the legacy `initialize` request); JSON's grammar treats a
            // newline between tokens as insignificant whitespace, so
            // collapsing it to a space here changes nothing the server parses.
            let single_line: String = request
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            writeln!(stdin, "{single_line}").unwrap();
        }
        drop(stdin);

        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "rllvm-query mcp exited with an error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Every response line of one multi-request session, parsed.
    fn mcp_session(
        scratch: &tempfile::TempDir,
        catalog: Option<&Path>,
        requests: &[&str],
    ) -> Vec<serde_json::Value> {
        let raw = mcp_raw_session(scratch, catalog, requests);
        raw.lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// One request against a preloaded catalog.
    fn mcp_raw_in(scratch: &tempfile::TempDir, catalog: &Path, request: &str) -> String {
        mcp_raw_session(scratch, Some(catalog), &[request])
    }

    /// The text payload of a `CallToolResult`, decoded as the JSON the tool
    /// answered with.
    fn tool_payload(response: &serde_json::Value) -> serde_json::Value {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool payload in {response:?}"));
        serde_json::from_str(text).unwrap()
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
        // invariant rather than searching for a place the two diverge. The one
        // deliberate divergence is `analysis.modules`: an MCP session reports
        // the roster once at `load_catalog`, so a query answer omits it, while
        // a standalone CLI invocation keeps it. Everything else is identical.
        let scratch = tempfile::tempdir().unwrap();
        let catalog = super::two_module_catalog(&scratch); // main calls add

        let cli: serde_json::Value = serde_json::from_slice(
            &Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
                .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
                .arg("--catalog")
                .arg(&catalog)
                .args(["--json", "callers", "add"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();

        let mcp = mcp_tool_call(&catalog, "callers", serde_json::json!({ "name": "add" }));

        assert_eq!(cli["results"], mcp["results"]);
        // The CLI keeps the per-module roster; the MCP answer drops it.
        assert_eq!(cli["analysis"]["modules"].as_array().unwrap().len(), 2);
        assert!(mcp["analysis"].get("modules").is_none());
        // Everything else in `analysis`, counts included, is identical.
        let mut cli_analysis = cli["analysis"].clone();
        cli_analysis.as_object_mut().unwrap().remove("modules");
        assert_eq!(cli_analysis, mcp["analysis"]);
        assert_eq!(cli["uncertainty"], mcp["uncertainty"]);
    }

    #[test]
    fn an_mcp_slice_emits_the_module_it_answers_for() {
        let scratch = tempfile::tempdir().unwrap();
        let catalog = super::archive_catalog_of(&scratch, "slice", super::SLICE_SOURCES);
        let out = scratch.path().join("mcp-slice.bc");
        let answer = mcp_tool_call(
            &catalog,
            "slice",
            serde_json::json!({ "from": "main", "to": "add", "emit_module": out.to_str().unwrap() }),
        );
        assert_eq!(answer["results"]["functions"].as_array().unwrap().len(), 3);
        assert_eq!(answer["emitted"]["path"], out.to_str().unwrap());
        assert_eq!(answer["emitted"]["functions"], 2);
        assert_eq!(
            super::defined_symbols(&out),
            BTreeSet::from(["add".to_string(), "main".to_string()])
        );
    }

    #[test]
    fn a_modern_request_is_served_without_a_handshake() {
        let response = mcp_exchange(&modern_request(
            "list-tools",
            "tools/list",
            serde_json::json!({}),
        ));
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(
            response["result"]["cacheScope"], "private",
            "modern cacheScope must be a spec-valid MCP enum value (public|private)"
        );
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

    /// One tool over the wire. That every one of the eleven is listed under a
    /// name `query_from_call` resolves is `mcp.rs`'s own
    /// `every_query_variant_is_listed_and_resolves_through_a_call`, which
    /// checks it against a match the compiler forces to stay exhaustive --
    /// driving the same eleven through a subprocess here proves nothing extra
    /// about the transport this test already covers.
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
    fn a_server_started_with_no_catalog_loads_one_and_answers_from_it() {
        // The point of the registry: the client chooses what to analyze.
        // Started bare, the server has nothing to answer from; after
        // `load_catalog` it answers about that program, in the same process.
        let scratch = tempfile::tempdir().unwrap();
        let catalog = super::two_module_catalog(&scratch); // main calls add

        let responses = mcp_session(
            &scratch,
            None,
            &[
                &modern_request(
                    "before",
                    "tools/call",
                    serde_json::json!({ "name": "callers", "arguments": { "name": "add" } }),
                ),
                &modern_request(
                    "load",
                    "tools/call",
                    serde_json::json!({
                        "name": "load_catalog",
                        "arguments": { "path": catalog.to_str().unwrap() }
                    }),
                ),
                &modern_request(
                    "after",
                    "tools/call",
                    serde_json::json!({ "name": "callers", "arguments": { "name": "add" } }),
                ),
            ],
        );
        assert_eq!(responses.len(), 3);

        assert_eq!(
            responses[0]["result"]["isError"], true,
            "a query before any load must not answer: {:?}",
            responses[0]
        );

        // The load reports what actually parsed, before anything is asked.
        assert_eq!(responses[1]["result"]["isError"], false);
        let loaded = tool_payload(&responses[1]);
        assert_eq!(loaded["analysis"]["analyzed"], 2);
        assert_eq!(loaded["scope"]["selected_entries"], 2);

        assert_eq!(responses[2]["result"]["isError"], false);
        let answer = tool_payload(&responses[2]);
        assert_eq!(answer["results"][0]["function"]["symbol"], "main");
    }

    #[test]
    fn inventory_loads_an_artifact_with_no_catalog_on_disk() {
        // No catalog JSON exists anywhere: the client hands the server a
        // captured artifact and queries what comes back. Nothing is written.
        let scratch = tempfile::tempdir().unwrap();
        let module = super::compile_bitcode(
            &scratch,
            "add.c",
            "int add(int a,int b){return a+b;}\nint main(void){ return add(2,3); }\n",
        );
        let responses = mcp_session(
            &scratch,
            None,
            &[
                &modern_request(
                    "inventory",
                    "tools/call",
                    serde_json::json!({
                        "name": "inventory",
                        "arguments": { "artifact": module.to_str().unwrap() }
                    }),
                ),
                &modern_request(
                    "query",
                    "tools/call",
                    serde_json::json!({ "name": "callers", "arguments": { "name": "add" } }),
                ),
            ],
        );

        assert_eq!(
            responses[0]["result"]["isError"], false,
            "inventory failed: {:?}",
            responses[0]
        );
        assert_eq!(tool_payload(&responses[0])["analysis"]["analyzed"], 1);

        assert_eq!(responses[1]["result"]["isError"], false);
        assert_eq!(
            tool_payload(&responses[1])["results"][0]["function"]["symbol"],
            "main"
        );

        // No catalog JSON was published anywhere: the whole point is that a
        // client can query an artifact without first naming a file to write.
        let published: Vec<PathBuf> = std::fs::read_dir(scratch.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        assert!(
            published.is_empty(),
            "inventory must not write a catalog to disk: {published:?}"
        );
    }

    #[test]
    fn two_catalogs_stay_loaded_and_are_told_apart() {
        let scratch = tempfile::tempdir().unwrap();
        let first = super::two_module_catalog(&scratch); // defines add and main
        let second = super::archive_catalog(&scratch); // defines one and two

        let responses = mcp_session(
            &scratch,
            None,
            &[
                &modern_request(
                    "load-1",
                    "tools/call",
                    serde_json::json!({
                        "name": "load_catalog",
                        "arguments": { "path": first.to_str().unwrap() }
                    }),
                ),
                &modern_request(
                    "load-2",
                    "tools/call",
                    serde_json::json!({
                        "name": "load_catalog",
                        "arguments": { "path": second.to_str().unwrap() }
                    }),
                ),
                &modern_request(
                    "ambiguous",
                    "tools/call",
                    serde_json::json!({ "name": "defs", "arguments": { "name": "add" } }),
                ),
                &modern_request(
                    "named",
                    "tools/call",
                    serde_json::json!({
                        "name": "defs",
                        "arguments": { "name": "add", "catalog": first.canonicalize().unwrap().to_str().unwrap() }
                    }),
                ),
                &modern_request(
                    "list",
                    "tools/call",
                    serde_json::json!({ "name": "list_catalogs", "arguments": {} }),
                ),
            ],
        );

        assert_eq!(
            responses[2]["result"]["isError"], true,
            "two loaded catalogs must not be silently picked between"
        );
        assert_eq!(responses[3]["result"]["isError"], false);
        assert_eq!(
            tool_payload(&responses[3])["results"][0]["function"]["symbol"],
            "add"
        );
        assert_eq!(
            tool_payload(&responses[4])["catalogs"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "both catalogs stay loaded across calls"
        );
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

    /// An agent's whole overlay loop in one server process: find the
    /// candidates, record an edge, walk it before saving, then save. Only the
    /// save writes, and it writes the header and the one record.
    #[test]
    fn an_mcp_session_records_saves_and_walks_an_overlay() {
        let scratch = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join("t.c"), FIELD_SOURCE).unwrap();
        let module = compile_bitcode_file(&scratch.path().join("t.c"), &["-O0", "-g"]);
        let catalog = plain_module_catalog(&scratch, &[module]);
        let overlay = scratch.path().join("plain-catalog.overlay.jsonl");
        let add = serde_json::json!({
            "op": "add",
            "via_field": { "record": "ops", "offset": 8 },
            "to": "h3",
            "confidence": "high",
            "provenance": ["init: o->on_event = h3"],
        });
        let call = |id: &str, name: &str, arguments: serde_json::Value| {
            modern_request(
                id,
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            )
        };

        let responses = mcp_session(
            &scratch,
            None,
            &[
                &call(
                    "load",
                    "load_catalog",
                    serde_json::json!({ "path": catalog.to_str().unwrap() }),
                ),
                &call("candidates", "resolution_candidates", serde_json::json!({})),
                &call(
                    "record",
                    "record_edges",
                    serde_json::json!({ "records": [add] }),
                ),
                &call(
                    "reach",
                    "reach",
                    serde_json::json!({ "from": "dispatch", "to": "h3", "include_overlay": true }),
                ),
                &call("save", "save_overlay", serde_json::json!({})),
            ],
        );
        assert_eq!(responses.len(), 5, "{responses:?}");
        for response in &responses {
            assert_eq!(response["result"]["isError"], false, "{response}");
        }

        let candidates = tool_payload(&responses[1]);
        assert!(
            candidates["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|group| {
                    group["field"] == serde_json::json!({ "record": "ops", "offset": 8 })
                }),
            "{candidates}"
        );
        assert_eq!(tool_payload(&responses[2])["pending"], 1);
        let reach = tool_payload(&responses[3]);
        assert_eq!(reach["uncertainty"]["agent_path_steps"], 1, "{reach}");
        let saved = tool_payload(&responses[4]);
        assert_eq!(saved["saved"], 1, "{saved}");

        let written = std::fs::read_to_string(&overlay).unwrap();
        assert_eq!(written.lines().count(), 2, "header and one add: {written}");
    }
} // mod mcp

#[test]
fn a_mistyped_location_is_rejected_before_the_catalog_is_analysed() {
    // `open` reads and extracts every selected module; a location that cannot
    // parse is knowable beforehand. Point the command at a catalog path that
    // does not exist: if validation ran first the error names the location,
    // and if it ran after `open` the error would name the missing catalog.
    let scratch = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .arg("--catalog")
        .arg(scratch.path().join("absent-catalog.json"))
        .args(["indirect-targets", "parser.c"])
        .env("RLLVM_CONFIG", scratch.path().join("config.toml"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("parser.c"),
        "the location, not the catalog, must be reported: {stderr}"
    );
}

/// The digest of the neutral facts extracted from `fixtures/facts-guard.ll`,
/// recorded with the `FACTS_FORMAT` it was taken under. A mismatch means
/// extraction output changed: bump `FACTS_FORMAT` in `src/cache.rs`, then
/// record the new pair here, or old cache entries will be served as if they
/// were current.
const FACTS_GUARD: (u32, &str) = (
    3,
    "32ff5d8f633b74d546a2dbcb727969112ab415a0f45d7c721384f42394cd846b",
);

/// Extracts the neutral facts from `fixtures/facts-guard.ll`: indirect calls,
/// dispatch-table uses, declarations, an alias, and the record fields an
/// indirect call, a store and named and literal-typed initializers name --
/// through a typed GEP, a C TBAA tag and a C++ (`_ZTS`) one -- none of which
/// the smaller equivalence fixtures above exercise.
fn guard_module_facts(scratch: &tempfile::TempDir) -> rllvm_query::ModuleFacts {
    let ir = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/facts-guard.ll"),
    )
    .unwrap();
    let loaded = rllvm_query::load::LoadedModule {
        id: "guard".into(),
        bytes: std::fs::read(assemble_ir(scratch, &ir)).unwrap(),
        record: Default::default(),
    };
    rllvm_query::extract::extract_neutral(&loaded).unwrap()
}

#[test]
fn the_facts_format_names_what_extraction_produces() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = guard_module_facts(&scratch);
    let digest = rllvm_core::catalog::hash_bytes(&serde_json::to_vec(&facts).unwrap());
    assert_eq!(
        (rllvm_query::cache::FACTS_FORMAT, digest.as_str()),
        FACTS_GUARD,
        "extraction output changed: bump FACTS_FORMAT and record ({}, \"{digest}\")",
        rllvm_query::cache::FACTS_FORMAT + 1,
    );
}

/// The guard fixture's facts round-trip through the cache unchanged: a
/// cache hit must answer exactly as fresh extraction does, including for the
/// indirect calls, dispatch-table uses and declarations this fixture adds
/// that the plain equivalence fixture (`a_warm_cache_answers_exactly_as_extraction_does`) lacks.
#[test]
fn the_guard_fixtures_facts_round_trip_through_the_cache() {
    let scratch = tempfile::tempdir().unwrap();
    let facts = guard_module_facts(&scratch);
    let cache = rllvm_query::FactsCache::new(scratch.path(), u64::MAX);
    cache.write("guard", &facts).unwrap();
    let read_back = cache.read("guard").expect("a written entry hits");
    assert_eq!(
        serde_json::to_value(&facts).unwrap(),
        serde_json::to_value(&read_back).unwrap()
    );

    // The entry carries field evidence of each basis the fixture produces,
    // so the comparison above covers it rather than passing on absences.
    let sites = read_back
        .call_sites
        .iter()
        .filter_map(|site| match &site.target {
            CallTarget::Indirect { via_field, .. } => via_field.as_ref(),
            _ => None,
        });
    let uses = read_back
        .uses
        .iter()
        .filter_map(|use_fact| use_fact.field.as_ref());
    let evidence: Vec<_> = sites.chain(uses).collect();
    let bases: Vec<_> = evidence.iter().map(|evidence| evidence.basis).collect();
    for basis in [
        rllvm_query::FieldBasis::StructGep,
        rllvm_query::FieldBasis::Tbaa,
        rllvm_query::FieldBasis::Initializer,
        rllvm_query::FieldBasis::DebugInfo,
    ] {
        assert!(bases.contains(&basis), "no {basis:?} field in {bases:?}");
    }
    // A C++ TBAA name (`_ZTSN1n3opsE`) reads as the record the IR names.
    assert!(
        evidence
            .iter()
            .any(|evidence| evidence.field.record == "n::ops"),
        "no `_ZTS` record normalized in {evidence:?}"
    );
}

/// `entry` calls `outer` through an always-inline helper, so the answers
/// carry an inlined frame; `outer` is defined in the second module.
const INLINED_AND_OUTER: &[(&str, &str)] = &[
    (
        "entry.c",
        "int outer(int);\n\
         static inline __attribute__((always_inline)) int add(int a,int b){ return outer(a+b); }\n\
         int entry(void){ return add(2,3); }\n",
    ),
    ("outer.c", "int outer(int x){ return x; }\n"),
];

fn scratch_cache(scratch: &tempfile::TempDir, warn_bytes: u64) -> rllvm_query::FactsCache {
    rllvm_query::FactsCache::new(&scratch.path().join("cache"), warn_bytes)
}

/// The `results` of the queries every cache test compares.
fn cache_probe_answers(session: &rllvm_query::Session) -> Vec<serde_json::Value> {
    use rllvm_query::Query;
    [
        Query::Defs {
            name: "outer".into(),
        },
        Query::Callers {
            name: "outer".into(),
        },
        Query::Callees {
            name: "entry".into(),
        },
    ]
    .iter()
    .map(|query| serde_json::to_value(&rllvm_query::run(session, query).unwrap().results).unwrap())
    .collect()
}

#[test]
fn a_warm_cache_answers_exactly_as_extraction_does() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let cache = scratch_cache(&scratch, u64::MAX);

    let uncached = rllvm_query::open(&catalog).unwrap();
    let cold = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    let warm = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();

    assert_eq!(uncached.cache_report(), None);
    let cold_report = cold.cache_report().unwrap();
    assert_eq!(
        (cold_report.hits, cold_report.misses, cold_report.written),
        (0, 2, 2)
    );
    let warm_report = warm.cache_report().unwrap();
    assert_eq!(
        (warm_report.hits, warm_report.misses, warm_report.written),
        (2, 0, 0)
    );

    let expected = cache_probe_answers(&uncached);
    assert_eq!(cache_probe_answers(&cold), expected);
    assert_eq!(cache_probe_answers(&warm), expected);
}

#[test]
fn an_edited_source_changes_status_on_a_warm_cache_without_a_miss() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = source_and_header(&scratch);
    let cache = scratch_cache(&scratch, u64::MAX);
    rllvm_query::open_with_cache(&fixture.catalog, Some(&cache)).unwrap();

    std::fs::write(&fixture.header, "int helper(int x){return x+2;}\n").unwrap();
    let session = rllvm_query::open_with_cache(&fixture.catalog, Some(&cache)).unwrap();

    let report = session.cache_report().unwrap();
    assert_eq!((report.hits, report.misses), (1, 0));
    let answer = serde_json::to_value(
        rllvm_query::run(
            &session,
            &rllvm_query::Query::Defs {
                name: "helper".into(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        answer["results"][0]["location"]["source_status"],
        "modified"
    );
}

#[test]
fn the_same_bitcode_in_another_catalog_hits_under_its_own_ids() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let cache = scratch_cache(&scratch, u64::MAX);
    rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();

    let mut renamed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&catalog).unwrap()).unwrap();
    for (index, module) in renamed["modules"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        module["id"] = format!("renamed-{index}").into();
    }
    let other = scratch.path().join("renamed-catalog.json");
    std::fs::write(&other, serde_json::to_vec(&renamed).unwrap()).unwrap();

    let session = rllvm_query::open_with_cache(&other, Some(&cache)).unwrap();
    assert_eq!(session.cache_report().unwrap().hits, 2);
    let answer = serde_json::to_value(
        rllvm_query::run(
            &session,
            &rllvm_query::Query::Defs {
                name: "outer".into(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    let module_id = answer["results"][0]["function"]["module_id"]
        .as_str()
        .unwrap();
    assert!(module_id.starts_with("renamed-"), "{module_id}");
}

#[test]
fn two_modules_with_identical_bytes_share_an_entry_but_not_an_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog, _) = write_catalog_with_one_module(&scratch);
    let mut doubled: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&catalog).unwrap()).unwrap();
    let mut copy = doubled["modules"][0].clone();
    copy["id"] = "add-copy".into();
    doubled["modules"].as_array_mut().unwrap().push(copy);
    // `read_catalog` checks `scope.selected_entries` against the actual
    // module count; the fixture starts at one real module, so the doubled
    // catalog must say so too, or the load is rejected before the cache is
    // ever consulted.
    doubled["scope"]["selected_entries"] = 2.into();
    doubled["scope"]["total_entries"] = 2.into();
    std::fs::write(&catalog, serde_json::to_vec(&doubled).unwrap()).unwrap();
    let cache = scratch_cache(&scratch, u64::MAX);

    let session = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    let report = session.cache_report().unwrap();
    assert_eq!((report.hits, report.misses, report.written), (1, 1, 1));
    let answer = serde_json::to_value(
        rllvm_query::run(&session, &rllvm_query::Query::Defs { name: "add".into() }).unwrap(),
    )
    .unwrap();
    let ids: BTreeSet<&str> = answer["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["function"]["module_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, BTreeSet::from(["add", "add-copy"]));
}

#[test]
fn a_catalog_without_content_hashes_bypasses_the_cache() {
    let scratch = tempfile::tempdir().unwrap();
    let (catalog_path, _) = write_catalog_with_one_module(&scratch);
    let mut catalog = read_catalog(&catalog_path).unwrap();
    catalog.modules[0].content_sha256 = None;
    let cache = scratch_cache(&scratch, u64::MAX);

    let session =
        rllvm_query::open_catalog_with_cache(catalog, scratch.path(), Some(&cache)).unwrap();
    let report = session.cache_report().unwrap();
    assert_eq!((report.hits, report.misses, report.written), (0, 0, 0));
    assert_eq!(cache.disk_bytes(), 0);
    let analysis = rllvm_query::run(&session, &rllvm_query::Query::Externals)
        .unwrap()
        .analysis;
    assert_eq!(analysis.analyzed, 1);
    assert_eq!(
        analysis.modules[0].status,
        rllvm_query::ModuleAnalysis::Analyzed
    );
}

#[test]
fn a_corrupt_entry_is_a_miss_that_still_answers_and_is_replaced() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let cache = scratch_cache(&scratch, u64::MAX);
    let expected =
        cache_probe_answers(&rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap());

    let generation = cache
        .directory()
        .join(rllvm_query::FactsCache::generation());
    for entry in std::fs::read_dir(&generation).unwrap() {
        std::fs::write(entry.unwrap().path(), b"junk").unwrap();
    }
    let recovered = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    assert_eq!(recovered.cache_report().unwrap().misses, 2);
    assert_eq!(cache_probe_answers(&recovered), expected);
    let again = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    assert_eq!(
        again.cache_report().unwrap().hits,
        2,
        "the junk was replaced"
    );
}

#[test]
fn an_unwritable_cache_still_answers() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let not_a_directory = scratch.path().join("file");
    std::fs::write(&not_a_directory, b"x").unwrap();
    let cache = rllvm_query::FactsCache::new(&not_a_directory, u64::MAX);

    let session = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    let report = session.cache_report().unwrap();
    assert_eq!((report.misses, report.written), (2, 0));
    assert_eq!(
        cache_probe_answers(&session),
        cache_probe_answers(&rllvm_query::open(&catalog).unwrap())
    );
}

/// Resolving the configured cache root must never create it, and never log
/// at error level when it cannot be created: `facts_cache()` runs before
/// every query, so a read-only HOME must not print an `ERROR` line on every
/// answer. `blocked/cache` can never be created because `blocked` is a file.
#[test]
fn an_uncreatable_configured_cache_dir_is_a_silent_miss() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let base_config = scratch_rllvm_config(scratch.path());
    let blocked = scratch.path().join("blocked");
    std::fs::write(&blocked, b"x").unwrap();
    let contents = std::fs::read_to_string(&base_config).unwrap();
    let contents: String = contents
        .lines()
        .map(|line| {
            if line.starts_with("cache_dir") {
                format!("cache_dir = '{}'\n", blocked.join("cache").display())
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    let config = scratch.path().join("uncreatable-cache-config.toml");
    std::fs::write(&config, contents).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", &config)
        .arg("--catalog")
        .arg(&catalog)
        .args(["--json", "callers", "add"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["analysis"]["cache"]["written"], 0);
}

#[test]
fn disk_use_counts_existing_and_new_entries_against_the_threshold() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let cache = scratch_cache(&scratch, 1);

    let cold = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    let report = cold.cache_report().unwrap();
    assert_eq!(
        report.disk_bytes,
        cache.disk_bytes(),
        "pre-existing zero plus both writes"
    );
    assert_eq!(report.warn_bytes, 1);
    assert!(report.over_threshold);

    let warm = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    assert_eq!(warm.cache_report().unwrap().disk_bytes, cache.disk_bytes());
}

#[test]
fn the_analysis_block_carries_the_cache_report_only_when_cached() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = archive_catalog_of(&scratch, "inlined", INLINED_AND_OUTER);
    let query = rllvm_query::Query::Defs {
        name: "outer".into(),
    };

    let plain = serde_json::to_value(
        rllvm_query::run(&rllvm_query::open(&catalog).unwrap(), &query).unwrap(),
    )
    .unwrap();
    assert!(plain["analysis"].get("cache").is_none());

    let cache = scratch_cache(&scratch, u64::MAX);
    let session = rllvm_query::open_with_cache(&catalog, Some(&cache)).unwrap();
    let cached = serde_json::to_value(rllvm_query::run(&session, &query).unwrap()).unwrap();
    assert_eq!(cached["analysis"]["cache"]["misses"], 2);
    assert_eq!(cached["schema_version"], 2);
}

#[test]
fn the_cli_fills_the_configured_cache_and_hits_it_next_time() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let cold = query_json(&scratch, &catalog, &["callers", "add"]);
    assert_eq!(cold["analysis"]["cache"]["written"], 2);
    let warm = query_json(&scratch, &catalog, &["callers", "add"]);
    assert_eq!(warm["analysis"]["cache"]["hits"], 2);
    assert_eq!(warm["results"], cold["results"]);
    assert!(
        scratch.path().join("cache/query-facts").is_dir(),
        "the scratch config must confine the cache"
    );
}

#[test]
fn rllvm_query_cache_0_turns_the_cache_off() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .env("RLLVM_QUERY_CACHE", "0")
        .arg("--catalog")
        .arg(&catalog)
        .args(["--json", "callers", "add"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(answer["analysis"].get("cache").is_none());
    assert!(!scratch.path().join("cache/query-facts").exists());
}

#[test]
fn an_mcp_load_reports_the_cache() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let mut registry =
        rllvm_query::mcp::Registry::with_cache(Some(scratch_cache(&scratch, u64::MAX)));
    let summary = registry.load(&catalog).unwrap();
    assert_eq!(summary["analysis"]["cache"]["misses"], 2);
}

#[test]
fn full_text_prints_the_cache_line() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let text = query_text(&scratch, &catalog, &["--full", "callers", "add"]);
    assert!(text.contains("cache: 0 hit, 2 miss, 2 written, "), "{text}");
}

fn query_cache_command(scratch: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_cache_command_reports_and_clears_without_a_catalog() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    query_json(&scratch, &catalog, &["callers", "add"]);

    let report = query_cache_command(&scratch, &["--json", "cache"]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let usage: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(usage["generations"][0]["entries"], 2);
    assert_eq!(usage["over_threshold"], false);

    let text = String::from_utf8(query_cache_command(&scratch, &["cache"]).stdout).unwrap();
    assert!(
        text.contains("current:") && text.contains("2 entries"),
        "{text}"
    );

    let cleared = query_cache_command(&scratch, &["cache", "clear"]);
    assert!(cleared.status.success());
    assert!(String::from_utf8_lossy(&cleared.stdout).contains("removed 2 entries"));
    let after: serde_json::Value =
        serde_json::from_slice(&query_cache_command(&scratch, &["--json", "cache"]).stdout)
            .unwrap();
    assert_eq!(after["total_bytes"], 0);
}

/// An abandoned `.tmp*` write sitting in the current generation is counted
/// separately from the entries `clear` removes, and named in the text
/// summary.
#[test]
fn clearing_the_cache_counts_orphaned_temp_files_in_its_summary() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    query_json(&scratch, &catalog, &["callers", "add"]);

    let generation = scratch
        .path()
        .join("cache/query-facts")
        .join(rllvm_query::FactsCache::generation());
    let orphan = generation.join(".tmpORPHAN1");
    std::fs::write(&orphan, [0u8; 3]).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 61);
    std::fs::File::open(&orphan)
        .unwrap()
        .set_modified(past)
        .unwrap();

    let cleared = query_cache_command(&scratch, &["cache", "clear"]);
    assert!(cleared.status.success());
    let text = String::from_utf8_lossy(&cleared.stdout);
    assert!(
        text.contains("removed 2 entries and 1 orphaned temp file(s)"),
        "{text}"
    );
    assert!(!orphan.exists());
}

/// `cache` reads no catalog, so `--catalog` alongside it is a mistake, not a
/// catalog to load -- even one that does not exist.
#[test]
fn the_cache_command_rejects_a_catalog() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = scratch.path().join("nonexistent.json");
    let output = query_cache_command(&scratch, &["--catalog", catalog.to_str().unwrap(), "cache"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("drop --catalog"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `rllvm-query cache` only reads: inspecting a cache that has never been
/// written must not bring its directory into existence.
#[test]
fn the_cache_command_does_not_create_the_cache_directory() {
    let scratch = tempfile::tempdir().unwrap();
    let cache_dir = scratch.path().join("cache"); // named by scratch_rllvm_config, never created

    let report = query_cache_command(&scratch, &["cache"]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    assert!(
        !cache_dir.exists(),
        "inspecting the cache must not create its directory"
    );
}

#[test]
fn an_answer_over_the_threshold_says_how_to_prune() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let config = scratch_rllvm_config(scratch.path());
    let mut contents = std::fs::read_to_string(&config).unwrap();
    contents.push_str("query_cache_warn_mb = 0\n");
    std::fs::write(&config, contents).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", &config)
        .arg("--catalog")
        .arg(&catalog)
        .args(["callers", "add"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("over query_cache_warn_mb (0 MB); prune with rllvm-query cache clear"),
        "{text}"
    );
}

/// What a compiler emits when two symbols share one body: `rustc` turns an
/// `extern "C"` wrapper that compiles to the same code as the method it calls
/// into an alias of that method. Module A defines the body and the alias and
/// calls through the alias itself; module B reaches it by name.
const ALIASED_MODULES: [(&str, &str); 2] = [
    (
        "aliased",
        r#"define void @leaf() {
  ret void
}

define void @impl() {
  call void @leaf()
  ret void
}

@exported = alias void (), ptr @impl

define void @local_caller() {
  call void @exported()
  ret void
}
"#,
    ),
    (
        "user",
        r#"declare void @exported()

define void @main() {
  call void @exported()
  ret void
}
"#,
    ),
];

fn aliased_catalog(scratch: &tempfile::TempDir) -> PathBuf {
    ir_catalog(scratch, &ALIASED_MODULES)
}

/// Assembles each `(name, IR)` into its own module and catalogs them all,
/// for shapes a C compiler will not produce on request.
fn ir_catalog(scratch: &tempfile::TempDir, ir_modules: &[(&str, &str)]) -> PathBuf {
    let modules: Vec<PathBuf> = ir_modules
        .iter()
        .map(|(name, ir)| {
            let directory = scratch.path().join(name);
            std::fs::create_dir(&directory).unwrap();
            let source = directory.join(format!("{name}.ll"));
            let module = directory.join(format!("{name}.bc"));
            std::fs::write(&source, ir).unwrap();
            let status = Command::new(llvm_bin("llvm-as"))
                .arg(&source)
                .arg("-o")
                .arg(&module)
                .status()
                .unwrap();
            assert!(status.success(), "llvm-as rejected {name}");
            module
        })
        .collect();
    plain_module_catalog(scratch, &modules)
}

#[test]
fn an_alias_is_a_definition_that_forwards_to_its_target() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = aliased_catalog(&scratch);

    let defs = query_json(&scratch, &catalog, &["defs", "exported"]);
    assert_eq!(
        defs["results"].as_array().map(Vec::len),
        Some(1),
        "the alias is defined in the captured code: {defs}"
    );

    let callees = query_json(&scratch, &catalog, &["callees", "local_caller"]);
    assert_eq!(
        callees["results"][0]["target"]["kind"], "direct",
        "a call through an alias names its callee: {callees}"
    );
    assert_eq!(
        callees["results"][0]["target"]["callee"]["symbol"],
        "exported"
    );

    let reach = query_json(&scratch, &catalog, &["reach", "main", "leaf"]);
    let steps = reach["results"]
        .as_array()
        .unwrap_or_else(|| panic!("a path runs through the alias: {reach}"));
    assert!(
        steps.iter().any(|step| step["kind"] == "alias"),
        "the path names the alias step: {reach}"
    );

    let callers = query_json(&scratch, &catalog, &["callers", "impl"]);
    let callers: Vec<&str> = callers["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|caller| caller["function"]["symbol"].as_str().unwrap())
        .collect();
    assert!(
        callers.contains(&"main") && callers.contains(&"local_caller"),
        "calls through an alias are calls to its target: {callers:?}"
    );
}

/// An alias has no body of its own, so a slice member that is an alias is
/// emitted with the function it stands for, even when that function is not
/// itself on the path; anything else stays out.
#[test]
fn an_emitted_alias_brings_the_body_it_stands_for() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = aliased_catalog(&scratch);
    for (to, defined) in [
        ("exported", &["exported", "impl", "main"][..]),
        ("leaf", &["exported", "impl", "leaf", "main"][..]),
    ] {
        let out = scratch.path().join(format!("{to}.bc"));
        let answer = query_json(
            &scratch,
            &catalog,
            &["slice", "main", to, "--emit-module", out.to_str().unwrap()],
        );
        assert_eq!(answer["emitted"]["functions"], defined.len(), "{answer}");
        assert_eq!(
            defined_symbols(&out),
            defined.iter().map(|symbol| symbol.to_string()).collect(),
            "slice main {to}"
        );
    }
}

#[test]
fn resolution_candidates_join_a_dispatch_to_its_stores() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(scratch.path().join("t.c"), FIELD_SOURCE).unwrap();
    let module = compile_bitcode_file(&scratch.path().join("t.c"), &["-O0", "-g"]);
    let catalog = plain_module_catalog(&scratch, &[module]);

    let value = query_json(&scratch, &catalog, &["resolution-candidates"]);
    let groups = value["results"].as_array().unwrap();
    let group = groups
        .iter()
        .find(|group| group["field"] == serde_json::json!({ "record": "ops", "offset": 8 }))
        .unwrap_or_else(|| panic!("no ops@8 group: {value}"));

    assert_eq!(group["field_name"], "on_event");
    let sites: Vec<_> = group["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|site| site["site"]["function"]["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(sites, ["dispatch"]);

    let candidates: Vec<_> = group["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["function"]["symbol"].as_str().unwrap())
        .collect();
    assert!(candidates.contains(&"h1"), "{candidates:?}");
    assert!(candidates.contains(&"h3"), "{candidates:?}");
    assert!(!candidates.contains(&"h2"), "{candidates:?}");
}

/// A walk over an overlay `--overlay` names reads that file: a missing one
/// is a mistyped path, not an empty overlay. Only the default path beside
/// the catalog may be missing, as it is before anything is recorded.
#[test]
fn a_walk_over_a_named_overlay_needs_the_file() {
    let scratch = tempfile::tempdir().unwrap();
    let catalog = two_module_catalog(&scratch);
    let missing = scratch.path().join("mistyped.overlay.jsonl");
    let walk = ["reach", "main", "add", "--include-overlay"];

    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .env("RLLVM_CONFIG", scratch_rllvm_config(scratch.path()))
        .arg("--catalog")
        .arg(&catalog)
        .arg("--overlay")
        .arg(&missing)
        .args(walk)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&missing.display().to_string()), "{stderr}");

    let named = ["--overlay", missing.to_str().unwrap()];
    let output = query_stdin(&scratch, &catalog, &named, &format!("{}\n", walk.join(" ")));
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&missing.display().to_string()), "{stderr}");
    assert!(!missing.exists(), "a walk never creates the file");

    let answer = query_text(&scratch, &catalog, &walk);
    assert!(answer.contains("main"), "{answer}");
}

#[test]
fn the_overlay_cli_records_lists_and_compacts() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(scratch.path().join("t.c"), FIELD_SOURCE).unwrap();
    let module = compile_bitcode_file(&scratch.path().join("t.c"), &["-O0", "-g"]);
    let catalog = plain_module_catalog(&scratch, &[module]);
    let overlay = scratch.path().join("plain-catalog.overlay.jsonl");
    let record = |input: String| {
        let output = query_stdin(&scratch, &catalog, &["overlay", "record"], &input);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
            stderr,
        )
    };

    let add = r#"{"op":"add","via_field":{"record":"ops","offset":8},"to":"h3","confidence":"high","provenance":["init: o->on_event = h3"]}"#;
    let (success, stdout, stderr) = record(format!("{add}\n"));
    assert!(success, "{stderr}");
    assert_eq!(
        stdout,
        format!("recorded 1, saved 1 to {}\n", overlay.display())
    );

    // One ungrounded record fails the whole batch, named by its stdin line
    // as a malformed one would be: blank lines count.
    let ungrounded = add.replace("\"offset\":8", "\"offset\":16");
    let before = std::fs::read(&overlay).unwrap();
    let (success, _, stderr) = record(format!("\n{add}\n{ungrounded}\n"));
    assert!(!success);
    assert!(stderr.contains("line 3: "), "{stderr}");
    assert_eq!(std::fs::read(&overlay).unwrap(), before);

    let listed = query_json(&scratch, &catalog, &["overlay", "list"]);
    let edges = listed["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1, "{listed}");
    assert_eq!(edges[0]["sites"], 1, "{listed}");
    assert_eq!(edges[0]["key"]["to"]["symbol"], "h3", "{listed}");
    let text = query_text(&scratch, &catalog, &["overlay", "list"]);
    assert!(text.starts_with("ops@8 -> h3 ["), "{text}");
    assert!(text.ends_with("]  high  unverified  1 site(s)\n"), "{text}");

    let retract = serde_json::json!({
        "op": "retract",
        "edge": edges[0]["key"],
        "reason": "init is never called",
    });
    let (success, _, stderr) = record(format!("{retract}\n"));
    assert!(success, "{stderr}");
    assert_eq!(
        query_text(&scratch, &catalog, &["overlay", "compact"]),
        format!("compacted {}: 0 edges\n", overlay.display())
    );
    let written = std::fs::read_to_string(&overlay).unwrap();
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(
        written.starts_with("{\"v\":1,\"fingerprint\":"),
        "{written}"
    );
}
