use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

fn llvm_bin(name: &str) -> PathBuf {
    let config = rllvm_core::utils::find_llvm_config().unwrap();
    let output = Command::new(config).arg("--bindir").output().unwrap();
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).unwrap().trim()).join(name)
}

fn rllvm(scratch: &TempDir) -> Command {
    let config = scratch.path().join("config.toml");
    fs::write(
        &config,
        "bitcode_generation_flags = ['-DUNEXPECTED_WRAPPER_OVERRIDE']\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rllvm-compdb"));
    command
        .env("RLLVM_CONFIG", config)
        .current_dir(scratch.path());
    command
}

fn write_database(scratch: &TempDir, entries: Value) {
    fs::write(
        scratch.path().join("compile_commands.json"),
        serde_json::to_vec_pretty(&entries).unwrap(),
    )
    .unwrap();
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn disassemble(path: &Path) -> String {
    let output = Command::new(llvm_bin("llvm-dis"))
        .arg(path)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[cfg(target_os = "macos")]
fn system_headers_infer_the_macos_sdk_without_environment_setup() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("system.c"),
        "#include <unistd.h>\nint system_headers(void){return STDOUT_FILENO;}\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"system.c","arguments":[llvm_bin("clang"),"-c","system.c"]}]),
    );
    let output = rllvm(&scratch)
        .env_remove("SDKROOT")
        .args(["generate", ".", "--output-dir", "analysis"])
        .output()
        .unwrap();
    assert_success(&output);
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    let module = &catalog["modules"][0];
    let sdk = module["compilation"]["environment"]["SDKROOT"]
        .as_str()
        .unwrap();
    assert!(Path::new(sdk).is_absolute() && Path::new(sdk).is_dir());
    assert!(
        !module["compilation"]["recorded_arguments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|argument| argument.as_str() == Some(sdk))
    );
    let ir = disassemble(
        &scratch
            .path()
            .join("analysis")
            .join(module["path"].as_str().unwrap()),
    );
    assert!(ir.contains("@system_headers") && ir.contains("ret i32 1"));
}

#[test]
fn explicit_sysroots_and_analysis_overrides_select_the_requested_headers() {
    let scratch = tempfile::tempdir().unwrap();
    let recorded_sdk = scratch.path().join("recorded-sdk");
    let override_sdk = scratch.path().join("override-sdk");
    for (sdk, value) in [(&recorded_sdk, 11), (&override_sdk, 17)] {
        fs::create_dir_all(sdk.join("usr/include")).unwrap();
        fs::write(
            sdk.join("usr/include/sdk_value.h"),
            format!("#define SDK_VALUE {value}\n"),
        )
        .unwrap();
    }
    fs::write(
        scratch.path().join("system.c"),
        "#include <sdk_value.h>\nint sdk_value(void){return SDK_VALUE;}\n",
    )
    .unwrap();
    // Response files must participate in SDK selection just like direct argv.
    fs::write(
        scratch.path().join("sdk.rsp"),
        format!("--sysroot=\"{}\"", recorded_sdk.display()),
    )
    .unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"system.c","arguments":[llvm_bin("clang"),"@sdk.rsp","-c","system.c"]}]),
    );
    let mut analysis_ids = Vec::new();
    for (name, value) in [("recorded", 11), ("override", 17)] {
        let mut command = rllvm(&scratch);
        command
            .env_remove("SDKROOT")
            .args(["generate", ".", "--output-dir", name]);
        if name == "override" {
            command.arg(format!("--extra-arg=--sysroot={}", override_sdk.display()));
        }
        assert_success(&command.output().unwrap());
        let output = scratch.path().join(name);
        let catalog: Value =
            serde_json::from_slice(&fs::read(output.join("catalog.json")).unwrap()).unwrap();
        let module = &catalog["modules"][0];
        assert!(module["compilation"]["environment"]["SDKROOT"].is_null());
        analysis_ids.push(module["compilation"]["analysis_id"].clone());
        let ir = disassemble(&output.join(module["path"].as_str().unwrap()));
        assert!(ir.contains(&format!("ret i32 {value}")));
    }
    assert_ne!(analysis_ids[0], analysis_ids[1]);
}

#[test]
#[cfg(target_os = "macos")]
fn sdk_discovery_failure_keeps_usable_entries_and_explains_the_override() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().unwrap();
    fs::create_dir(scratch.path().join("bin")).unwrap();
    let xcrun = scratch.path().join("bin/xcrun");
    fs::write(
        &xcrun,
        "#!/bin/sh\nprintf 'called\\n' >> xcrun-calls\nprintf 'SDK unavailable\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&xcrun, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        scratch.path().join("system.c"),
        "#include <unistd.h>\nint sdk_value(void){return STDOUT_FILENO;}\n",
    )
    .unwrap();
    fs::write(
        scratch.path().join("simple.c"),
        "int simple(void){return 7;}\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([
            {"directory":".","file":"system.c","arguments":[llvm_bin("clang"),"-c","system.c"]},
            {"directory":".","file":"simple.c","arguments":[llvm_bin("clang"),"-c","simple.c"]}
        ]),
    );
    let path = std::env::join_paths(
        std::iter::once(scratch.path().join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = rllvm(&scratch)
        .env_remove("SDKROOT")
        .env("PATH", path)
        .args(["generate", ".", "--output-dir", "analysis", "--jobs", "2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("set SDKROOT"));
    assert_eq!(
        fs::read_to_string(scratch.path().join("xcrun-calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    assert_eq!(catalog["modules"][0]["status"], "failed");
    assert_eq!(catalog["modules"][1]["status"], "available");
}

#[test]
fn system_headers_use_the_explicit_sdk_environment() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("system.c"),
        "#include <stdlib.h>\nint system_headers(void){return EXIT_SUCCESS;}\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"system.c","arguments":[llvm_bin("clang"),"-c","system.c"]}]),
    );
    let sdk = if cfg!(target_os = "macos") {
        let output = Command::new("xcrun")
            .arg("--show-sdk-path")
            .output()
            .unwrap();
        assert_success(&output);
        Some(String::from_utf8(output.stdout).unwrap().trim().to_owned())
    } else {
        None
    };
    let mut command = rllvm(&scratch);
    if let Some(sdk) = &sdk {
        command.env("SDKROOT", sdk);
    }
    let output = command
        .args(["generate", ".", "--output-dir", "analysis"])
        .output()
        .unwrap();
    assert_success(&output);
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    let module = &catalog["modules"][0];
    if let Some(sdk) = sdk {
        assert_eq!(module["compilation"]["environment"]["SDKROOT"], sdk);
    }
    let ir = disassemble(
        &scratch
            .path()
            .join("analysis")
            .join(module["path"].as_str().unwrap()),
    );
    assert!(ir.contains("@system_headers") && ir.contains("ret i32 0"));
}

#[test]
fn selected_current_source_generates_ir_and_preserves_native_outputs() {
    let scratch = tempfile::tempdir().unwrap();
    fs::create_dir(scratch.path().join("build")).unwrap();
    fs::write(scratch.path().join("build/source.c"), "#ifdef UNEXPECTED_WRAPPER_OVERRIDE\n#error wrapper settings leaked\n#endif\nint selected(void) { return VALUE; }\n").unwrap();
    fs::write(scratch.path().join("build/original.o"), "original object").unwrap();
    fs::write(scratch.path().join("build/original.d"), "original deps").unwrap();
    fs::write(
        scratch.path().join("build/compile.rsp"),
        "-O2 -DVALUE=17 -c source.c -ooriginal.o -MMD -MForiginal.d",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([
            {"directory":"build", "file":"source.c", "arguments":[llvm_bin("clang"), "@compile.rsp"]},
            {"directory":"build", "file":"missing.c", "arguments":[llvm_bin("clang"), "-c", "missing.c"]}
        ]),
    );
    let output = rllvm(&scratch)
        .args([
            "generate",
            ".",
            "--source",
            "build/source.c",
            "--output-dir",
            "analysis",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    assert_eq!(catalog["scope"]["selected_entries"], 1);
    assert_eq!(catalog["scope"]["total_entries"], 2);
    assert!(catalog["scope"]["whole_program_complete"].is_null());
    let module = &catalog["modules"][0];
    assert_eq!(module["status"], "available");
    assert!(module["target_triple"].as_str().unwrap().len() > 5);
    assert!(!module["data_layout"].as_str().unwrap().is_empty());
    assert_eq!(module["content_sha256"].as_str().unwrap().len(), 64);
    let digest = &module["sources"][0]["digest"];
    assert_eq!(digest["value"].as_str().unwrap().len(), 64);
    assert_eq!(digest["algorithm"], "sha256");
    assert_eq!(
        digest["origin"], "capture",
        "compdb runs the compiler itself, so its digest describes the build"
    );
    let path = Path::new(module["path"].as_str().unwrap());
    assert!(path.is_relative());
    let ir = disassemble(&scratch.path().join("analysis").join(path));
    assert!(ir.contains("@selected"));
    assert!(ir.contains("ret i32 17"));
    assert_eq!(
        fs::read_to_string(scratch.path().join("build/original.o")).unwrap(),
        "original object"
    );
    assert_eq!(
        fs::read_to_string(scratch.path().join("build/original.d")).unwrap(),
        "original deps"
    );
}

#[test]
fn failed_entries_keep_successes_and_catalog_order_with_separate_directories() {
    let scratch = tempfile::tempdir().unwrap();
    for (directory, value) in [("one", 11), ("two", 22)] {
        fs::create_dir(scratch.path().join(directory)).unwrap();
        fs::write(
            scratch.path().join(directory).join("source.cpp"),
            format!("extern \"C\" int value(void) {{ return {value}; }}\n"),
        )
        .unwrap();
    }
    write_database(
        &scratch,
        json!([
            {"directory":"one", "file":"source.cpp", "arguments":[llvm_bin("clang++"), "-c", "source.cpp"]},
            {"directory":"two", "file":"source.cpp", "arguments":[llvm_bin("clang++"), "-c", "source.cpp"]},
            {"directory":"one", "file":"generated.c", "arguments":[llvm_bin("clang"), "-c", "generated.c"]},
            {"directory":"two", "file":"source.cpp", "arguments":["ccache", "clang++", "-c", "source.cpp"]}
        ]),
    );
    let output = rllvm(&scratch)
        .args(["generate", ".", "--jobs", "2", "--output-dir", "analysis"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    for (index, value) in [11, 22].into_iter().enumerate() {
        let module = &catalog["modules"][index];
        assert_eq!(module["compilation"]["entry_index"], index);
        assert_eq!(module["status"], "available");
        let ir = disassemble(
            &scratch
                .path()
                .join("analysis")
                .join(module["path"].as_str().unwrap()),
        );
        assert!(ir.contains(&format!("ret i32 {value}")));
    }
    assert_eq!(catalog["modules"][2]["status"], "failed");
    assert_eq!(catalog["modules"][3]["status"], "unsupported");
    assert!(
        scratch
            .path()
            .join("analysis")
            .join(catalog["modules"][2]["diagnostic_path"].as_str().unwrap())
            .is_file()
    );
    assert!(
        !fs::read_dir(scratch.path().join("analysis"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp"))
    );
}

#[test]
fn explicit_overrides_and_current_source_edits_have_separate_identity() {
    let scratch = tempfile::tempdir().unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-O2","-DVALUE=1","-c","source.c"]}]),
    );
    // The source appears after the database, as generated inputs often do.
    fs::write(
        scratch.path().join("source.c"),
        "int value(void) { return VALUE; }\n",
    )
    .unwrap();
    let mut catalogs = Vec::new();
    for (directory, extra) in [
        ("default", None),
        ("override", Some("-DVALUE=29")),
        ("edited", None),
    ] {
        if directory == "edited" {
            fs::write(
                scratch.path().join("source.c"),
                "int value(void) { return VALUE + 4; }\n",
            )
            .unwrap();
        }
        let mut command = rllvm(&scratch);
        command.args(["generate", ".", "--output-dir", directory]);
        if let Some(extra) = extra {
            command.arg(format!("--extra-arg={extra}"));
        }
        assert_success(&command.output().unwrap());
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join(directory).join("catalog.json")).unwrap(),
        )
        .unwrap();
        let ir = disassemble(
            &scratch
                .path()
                .join(directory)
                .join(catalog["modules"][0]["path"].as_str().unwrap()),
        );
        let expected = match directory {
            "default" => 1,
            "override" => 29,
            _ => 5,
        };
        assert!(ir.contains(&format!("ret i32 {expected}")));
        catalogs.push(catalog);
    }
    let first = &catalogs[0]["modules"][0];
    let overridden = &catalogs[1]["modules"][0];
    let edited = &catalogs[2]["modules"][0];
    assert_eq!(first["configuration_id"], overridden["configuration_id"]);
    assert_ne!(
        first["compilation"]["analysis_id"],
        overridden["compilation"]["analysis_id"]
    );
    assert_ne!(first["content_sha256"], overridden["content_sha256"]);
    assert_ne!(
        first["sources"][0]["digest"]["value"],
        edited["sources"][0]["digest"]["value"]
    );
}

#[test]
fn existing_output_and_unmatched_selection_are_rejected_without_writing() {
    let scratch = tempfile::tempdir().unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c"]}]),
    );
    fs::create_dir(scratch.path().join("existing")).unwrap();
    fs::write(scratch.path().join("existing/keep"), "untouched").unwrap();
    let existing = rllvm(&scratch)
        .args(["generate", ".", "--output-dir", "existing"])
        .output()
        .unwrap();
    assert!(!existing.status.success());
    assert!(String::from_utf8_lossy(&existing.stderr).contains("must be new"));
    assert_eq!(
        fs::read_to_string(scratch.path().join("existing/keep")).unwrap(),
        "untouched"
    );
    assert_eq!(
        fs::read_dir(scratch.path().join("existing"))
            .unwrap()
            .count(),
        1
    );
    let unmatched = rllvm(&scratch)
        .args([
            "generate",
            ".",
            "--source",
            "absent.c",
            "--output-dir",
            "analysis",
        ])
        .output()
        .unwrap();
    assert!(!unmatched.status.success());
    assert!(!scratch.path().join("analysis").exists());
}

#[test]
fn unsafe_recorded_and_override_outputs_are_reported_without_touching_build_tree() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("source.c"),
        "int value(void) { return 3; }\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([
            {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c","-save-temps"]},
            {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c"]},
            {"malformed": true}
        ]),
    );
    let output = rllvm(&scratch)
        .args([
            "generate",
            ".",
            "--extra-arg=-ftime-trace=unowned.json",
            "--output-dir",
            "analysis",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    assert!(
        catalog["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|module| module["status"] == "unsupported")
    );
    assert_eq!(
        catalog["scope"]["analysis_arguments"],
        json!(["-ftime-trace=unowned.json"])
    );
    for name in ["source.i", "source.o", "source.bc", "unowned.json"] {
        assert!(!scratch.path().join(name).exists());
    }
}

#[test]
fn list_needs_neither_source_nor_compiler_and_completions_include_compdb() {
    let scratch = tempfile::tempdir().unwrap();
    write_database(
        &scratch,
        json!([{"directory":"absent", "file":"source.c", "command":"/missing/clang -c 'source.c' -o source.o"}]),
    );
    let output = rllvm(&scratch).args(["list", "."]).output().unwrap();
    assert_success(&output);
    let catalog: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(catalog["modules"][0]["status"], "planned");
    assert!(!scratch.path().join("absent").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-completions"))
        .args(["--shell", "bash", "--bin", "compdb"])
        .output()
        .unwrap();
    assert_success(&output);
    let completion = String::from_utf8(output.stdout).unwrap();
    assert!(completion.contains("rllvm-compdb"));
    assert!(completion.contains("--output-dir"));
}

#[test]
fn ordinary_wrapper_ignores_a_compilation_database() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("compile_commands.json"),
        "invalid database",
    )
    .unwrap();
    fs::write(
        scratch.path().join("source.c"),
        "int main(void) { return 0; }\n",
    )
    .unwrap();
    let config = scratch.path().join("wrapper.toml");
    fs::write(&config, format!("llvm_config_filepath = '{}'\nclang_filepath = '{}'\nclangxx_filepath = '{}'\nllvm_objcopy_filepath = '{}'\nllvm_ar_filepath = '{}'\nllvm_link_filepath = '{}'\n", rllvm_core::utils::find_llvm_config().unwrap().display(), llvm_bin("clang").display(), llvm_bin("clang++").display(), llvm_bin("llvm-objcopy").display(), llvm_bin("llvm-ar").display(), llvm_bin("llvm-link").display())).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rllvm-cc"))
        .env("RLLVM_CONFIG", &config)
        .current_dir(scratch.path())
        .args(["source.c", "-o", "app"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_success(&Command::new(scratch.path().join("app")).output().unwrap());
    assert_eq!(
        fs::read_to_string(scratch.path().join("compile_commands.json")).unwrap(),
        "invalid database"
    );
    assert!(!scratch.path().join("catalog.json").exists());
}

#[test]
fn analysis_identity_tracks_expanded_override_response_contents() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("source.c"),
        "int value(void) { return VALUE; }\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([{"directory":".", "file":"source.c", "arguments":[llvm_bin("clang"), "-c", "source.c"]}]),
    );
    let mut modules = Vec::new();
    for (output, value) in [("first", 1), ("second", 2)] {
        fs::write(
            scratch.path().join("analysis.rsp"),
            format!("-DVALUE={value}"),
        )
        .unwrap();
        assert_success(
            &rllvm(&scratch)
                .args([
                    "generate",
                    ".",
                    "--extra-arg=@analysis.rsp",
                    "--output-dir",
                    output,
                ])
                .output()
                .unwrap(),
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join(output).join("catalog.json")).unwrap(),
        )
        .unwrap();
        modules.push(catalog["modules"][0].clone());
    }
    assert_eq!(
        modules[0]["configuration_id"],
        modules[1]["configuration_id"]
    );
    assert_ne!(
        modules[0]["compilation"]["analysis_id"],
        modules[1]["compilation"]["analysis_id"]
    );
}

#[test]
fn compiler_metadata_is_resolved_once_per_distinct_driver() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let driver = scratch.path().join("clang");
    let log = scratch.path().join("versions.log");
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"));
    fs::write(
        &driver,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo version >> {}; fi\nexec {} \"$@\"\n",
            quote(&log),
            quote(&llvm_bin("clang"))
        ),
    )
    .unwrap();
    fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        scratch.path().join("source.c"),
        "int value(void) { return 2; }\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([
            {"directory":".", "file":"source.c", "arguments":[driver, "-c", "source.c"]},
            {"directory":".", "file":"source.c", "arguments":[driver, "-O2", "-c", "source.c"]}
        ]),
    );
    assert_success(
        &rllvm(&scratch)
            .args(["generate", ".", "--jobs", "2", "--output-dir", "analysis"])
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read_to_string(log).unwrap(), "version\n");
}

#[test]
fn parent_components_after_symlinks_preserve_recorded_directory_semantics() {
    let scratch = tempfile::tempdir().unwrap();
    fs::create_dir_all(scratch.path().join("actual/nested")).unwrap();
    std::os::unix::fs::symlink(
        scratch.path().join("actual/nested"),
        scratch.path().join("link"),
    )
    .unwrap();
    fs::write(
        scratch.path().join("source.c"),
        "int value(void) { return 19; }\n",
    )
    .unwrap();
    fs::write(
        scratch.path().join("actual/source.c"),
        "int value(void) { return 7; }\n",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([{"directory":"link/..", "file":"source.c", "arguments":[llvm_bin("clang"), "-c", "source.c"]}]),
    );
    assert_success(
        &rllvm(&scratch)
            .args(["generate", ".", "--output-dir", "analysis"])
            .output()
            .unwrap(),
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    let ir = disassemble(
        &scratch
            .path()
            .join("analysis")
            .join(catalog["modules"][0]["path"].as_str().unwrap()),
    );
    assert!(
        ir.contains("ret i32 7"),
        "wrong source compiled through symlink/.."
    );
}

#[test]
fn imported_save_stats_options_cannot_overwrite_build_statistics() {
    for flag in [
        "-save-stats",
        "-save-stats=cwd",
        "-save-stats=obj",
        "--save-stats",
        "--save-stats=cwd",
        "--save-stats=obj",
    ] {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(
            scratch.path().join("source.c"),
            "int value(void) { return 1; }\n",
        )
        .unwrap();
        fs::write(scratch.path().join("source.stats"), "original statistics").unwrap();
        write_database(
            &scratch,
            json!([
                {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c",flag]},
                {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c"]}
            ]),
        );
        let output = rllvm(&scratch)
            .args(["generate", ".", "--output-dir", "analysis"])
            .arg(format!("--extra-arg={flag}"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {flag}");
        assert_eq!(
            fs::read_to_string(scratch.path().join("source.stats")).unwrap(),
            "original statistics"
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join("analysis/catalog.json")).unwrap(),
        )
        .unwrap();
        assert!(
            catalog["modules"]
                .as_array()
                .unwrap()
                .iter()
                .all(|module| module["status"] == "unsupported")
        );
    }
}

#[test]
fn imported_language_matches_the_language_active_at_the_source() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(scratch.path().join("source.c"), "#ifdef __cplusplus\nextern \"C\" int language(void) { return 1; }\n#else\nint language(void) { return 2; }\n#endif\n").unwrap();
    fs::write(
        scratch.path().join("command.rsp"),
        "-x c++ -c source.c -x none",
    )
    .unwrap();
    write_database(
        &scratch,
        json!([
            {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-x","c++","-c","source.c","-x","none"]},
            {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"@command.rsp"]},
            {"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-xc++","-c","source.c","-xnone"]}
        ]),
    );
    let native = Command::new(llvm_bin("clang"))
        .current_dir(scratch.path())
        .args([
            "-x",
            "c++",
            "-c",
            "source.c",
            "-x",
            "none",
            "-emit-llvm",
            "-o",
            "expected.bc",
        ])
        .output()
        .unwrap();
    assert_success(&native);
    assert!(disassemble(&scratch.path().join("expected.bc")).contains("ret i32 1"));
    assert_success(
        &rllvm(&scratch)
            .args(["generate", ".", "--output-dir", "analysis"])
            .output()
            .unwrap(),
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    for module in catalog["modules"].as_array().unwrap() {
        let ir = disassemble(
            &scratch
                .path()
                .join("analysis")
                .join(module["path"].as_str().unwrap()),
        );
        assert!(
            ir.contains("ret i32 1"),
            "import changed positional language"
        );
    }
    fs::write(scratch.path().join("analysis.rsp"), "-x c").unwrap();
    assert_success(
        &rllvm(&scratch)
            .args([
                "generate",
                ".",
                "--extra-arg=@analysis.rsp",
                "--output-dir",
                "override",
            ])
            .output()
            .unwrap(),
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("override/catalog.json")).unwrap())
            .unwrap();
    for module in catalog["modules"].as_array().unwrap() {
        let ir = disassemble(
            &scratch
                .path()
                .join("override")
                .join(module["path"].as_str().unwrap()),
        );
        assert!(
            ir.contains("ret i32 2"),
            "explicit analysis language override was ignored"
        );
    }
}

#[test]
fn imported_compilers_receive_only_the_recorded_analysis_environment() {
    let scratch = tempfile::tempdir().unwrap();
    fs::create_dir(scratch.path().join("include")).unwrap();
    fs::write(scratch.path().join("include/value.h"), "#define VALUE 13\n").unwrap();
    fs::write(scratch.path().join("source.c"), "#include <value.h>\n#ifdef AMBIENT_OVERRIDE\n#error ambient override reached compiler\n#endif\nint value(void) { return VALUE; }\n").unwrap();
    fs::write(scratch.path().join("build.log"), "original build log").unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"source.c","arguments":[llvm_bin("clang"),"-c","source.c"]}]),
    );
    for (output_dir, override_value) in [("print", None), ("override", Some("+-DAMBIENT_OVERRIDE"))]
    {
        let mut command = rllvm(&scratch);
        command
            .env("CPATH", scratch.path().join("include"))
            .env("CC_PRINT_OPTIONS", "1")
            .env("CC_PRINT_OPTIONS_FILE", scratch.path().join("build.log"))
            .env("RLLVM_COMPDB_PRIVATE_SENTINEL", "must not reach compiler")
            .args(["generate", ".", "--output-dir", output_dir]);
        if let Some(value) = override_value {
            command.env("CCC_OVERRIDE_OPTIONS", value);
        }
        let output = command.output().unwrap();
        assert_success(&output);
        assert_eq!(
            fs::read_to_string(scratch.path().join("build.log")).unwrap(),
            "original build log"
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join(output_dir).join("catalog.json")).unwrap(),
        )
        .unwrap();
        let compilation = &catalog["modules"][0]["compilation"];
        assert_eq!(compilation["environment_complete"], true);
        assert_eq!(
            compilation["environment"]["CPATH"],
            scratch.path().join("include").to_str().unwrap()
        );
        for name in [
            "CC_PRINT_OPTIONS",
            "CC_PRINT_OPTIONS_FILE",
            "CCC_OVERRIDE_OPTIONS",
            "RLLVM_COMPDB_PRIVATE_SENTINEL",
        ] {
            assert!(compilation["environment"][name].is_null());
        }
        let ir = disassemble(
            &scratch
                .path()
                .join(output_dir)
                .join(catalog["modules"][0]["path"].as_str().unwrap()),
        );
        assert!(ir.contains("ret i32 13"));
    }
}

#[test]
fn imported_driver_default_configs_cannot_add_hidden_outputs_or_flags() {
    let scratch = tempfile::tempdir().unwrap();
    let driver = scratch.path().join("clang");
    std::os::unix::fs::symlink(llvm_bin("clang"), &driver).unwrap();
    fs::write(
        scratch.path().join("clang.cfg"),
        "-save-stats\n-DHIDDEN_CONFIG\n",
    )
    .unwrap();
    fs::write(scratch.path().join("source.stats"), "original statistics").unwrap();
    fs::write(scratch.path().join("source.c"), "#ifdef HIDDEN_CONFIG\n#error default config reached compiler\n#endif\nint value(void) { return 3; }\n").unwrap();
    write_database(
        &scratch,
        json!([{"directory":".","file":"source.c","arguments":[driver,"-no-canonical-prefixes","-c","source.c"]}]),
    );
    assert_success(
        &rllvm(&scratch)
            .args(["generate", ".", "--output-dir", "analysis"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read_to_string(scratch.path().join("source.stats")).unwrap(),
        "original statistics"
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(scratch.path().join("analysis/catalog.json")).unwrap())
            .unwrap();
    assert!(
        catalog["modules"][0]["compilation"]["effective_arguments"]
            .as_array()
            .unwrap()
            .contains(&json!("--no-default-config"))
    );
}

#[test]
fn imported_inactive_invalid_languages_keep_native_compiler_failures() {
    for arguments in [
        vec!["-c", "source.c", "-x", "bogus"],
        vec!["-x", "bogus", "-x", "c", "-c", "source.c"],
        vec!["-c", "source.c", "-xbogus"],
    ] {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(
            scratch.path().join("source.c"),
            "int value(void) { return 1; }\n",
        )
        .unwrap();
        let native = Command::new(llvm_bin("clang"))
            .current_dir(scratch.path())
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!native.status.success());
        assert!(String::from_utf8_lossy(&native.stderr).contains("language not recognized"));
        let argv: Vec<_> = std::iter::once(llvm_bin("clang").display().to_string())
            .chain(arguments.into_iter().map(String::from))
            .collect();
        write_database(
            &scratch,
            json!([{"directory":".","file":"source.c","arguments":argv}]),
        );
        let output = rllvm(&scratch)
            .args(["generate", ".", "--output-dir", "analysis"])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "import erased an invalid language option"
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join("analysis/catalog.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(catalog["modules"][0]["status"], "available");
        let diagnostic = fs::read_to_string(
            scratch
                .path()
                .join("analysis")
                .join(catalog["modules"][0]["diagnostic_path"].as_str().unwrap()),
        )
        .unwrap();
        assert!(diagnostic.contains("language not recognized"));
    }
}

#[test]
fn imported_language_restoration_preserves_inactive_valid_options_and_operands() {
    for include_directory in ["-x", "-xbogus"] {
        let scratch = tempfile::tempdir().unwrap();
        fs::create_dir(scratch.path().join(include_directory)).unwrap();
        fs::write(
            scratch.path().join(include_directory).join("value.h"),
            "#define VALUE 11\n",
        )
        .unwrap();
        fs::write(
            scratch.path().join("source.c"),
            "#include <value.h>\nint value(void) { return VALUE; }\n",
        )
        .unwrap();
        let arguments = ["-I", include_directory, "-c", "source.c", "-x", "assembler"];
        let native = Command::new(llvm_bin("clang"))
            .current_dir(scratch.path())
            .args(arguments)
            .args(["-emit-llvm", "-o", "expected.bc"])
            .output()
            .unwrap();
        assert_success(&native);
        assert!(disassemble(&scratch.path().join("expected.bc")).contains("ret i32 11"));
        let argv: Vec<_> = std::iter::once(llvm_bin("clang").display().to_string())
            .chain(arguments.into_iter().map(String::from))
            .collect();
        write_database(
            &scratch,
            json!([{"directory":".","file":"source.c","arguments":argv}]),
        );
        assert_success(
            &rllvm(&scratch)
                .args(["generate", ".", "--output-dir", "analysis"])
                .output()
                .unwrap(),
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(scratch.path().join("analysis/catalog.json")).unwrap(),
        )
        .unwrap();
        let ir = disassemble(
            &scratch
                .path()
                .join("analysis")
                .join(catalog["modules"][0]["path"].as_str().unwrap()),
        );
        assert!(ir.contains("ret i32 11"));
    }
}
