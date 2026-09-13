use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rllvm::catalog::{ModuleCatalog, ModuleStatus, write_catalog};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    tools: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let llvm = rllvm::utils::find_llvm_config().unwrap();
        let output = Command::new(llvm).arg("--bindir").output().unwrap();
        assert!(output.status.success());
        let tools = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let config = root.path().join("config.toml");
        fs::write(&config, format!("clang_filepath = '{}'\nclangxx_filepath = '{}'\nllvm_link_filepath = '{}'\nllvm_ar_filepath = '{}'\nllvm_config_filepath = '{}'\n",tools.join("clang").display(),tools.join("clang++").display(),tools.join("llvm-link").display(),tools.join("llvm-ar").display(),tools.join("llvm-config").display())).unwrap();
        Self {
            root,
            tools,
            config,
        }
    }

    fn module(&self, name: &str, source: &str, code: &str) -> PathBuf {
        let input = self.root.path().join(source);
        fs::write(&input, code).unwrap();
        let output = self.root.path().join(format!("{name}.bc"));
        let status = Command::new(self.tools.join("clang"))
            .args(["-emit-llvm", "-c", "-g", "-O0"])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
        output
    }

    fn object(&self, paths: &[PathBuf]) -> PathBuf {
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
        let references = paths
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect::<String>();
        object.append_section_data(section, references.as_bytes(), 1);
        let path = self.root.path().join("input.o");
        fs::write(&path, object.write().unwrap()).unwrap();
        path
    }

    fn command(&self, bin: &str) -> Command {
        let mut command = Command::new(match bin {
            "info" => env!("CARGO_BIN_EXE_rllvm-info"),
            "get" => env!("CARGO_BIN_EXE_rllvm-get-bc"),
            _ => unreachable!(),
        });
        command
            .current_dir(self.root.path())
            .env("RLLVM_CONFIG", &self.config)
            .env("LLVM_CONFIG", self.tools.join("llvm-config"));
        command
    }

    fn catalog(output: &Output) -> ModuleCatalog {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "invalid JSON: {e}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn ir(&self, input: &Path) -> String {
        let output = Command::new(self.tools.join("llvm-dis"))
            .arg(input)
            .args(["-o", "-"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}

#[test]
fn json_inventory_retains_missing_modules_and_unknown_legacy_metadata() {
    let f = Fixture::new();
    let a = f.module("first", "first.c", "int first(void){return 1;}");
    let b = f.module("second", "second.c", "int second(void){return 2;}");
    let input = f.object(&[a, b, f.root.path().join("missing.bc")]);
    let before = fs::read(&input).unwrap();
    let output = f
        .command("info")
        .arg(&input)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let catalog = Fixture::catalog(&output);
    assert_eq!(catalog.modules.len(), 3);
    assert_eq!(
        catalog
            .modules
            .iter()
            .filter(|m| m.status == ModuleStatus::Missing)
            .count(),
        1
    );
    assert!(catalog.modules.iter().all(|m| m.compiler.is_none()
        && m.configuration_id.is_none()
        && m.source_snapshot.is_none()));
    assert!(
        catalog
            .modules
            .iter()
            .filter(|m| m.status == ModuleStatus::Available)
            .all(|m| m.target_triple.is_some()
                && m.data_layout.is_some()
                && m.debug_info == Some(true)
                && !m.sources.is_empty())
    );
    assert!(catalog.scope.whole_program_complete.is_none());
    assert_eq!(fs::read(&input).unwrap(), before);
}

#[test]
fn selected_modules_relocate_without_merging_and_can_later_be_merged() {
    let f = Fixture::new();
    let a = f.module("first", "first.c", "int first(void){return 1;}");
    let b = f.module("second", "second.c", "int second(void){return 2;}");
    let input = f.object(&[a, b]);
    let catalog = Fixture::catalog(
        &f.command("info")
            .arg(&input)
            .arg("--json")
            .output()
            .unwrap(),
    );
    let module = catalog
        .modules
        .iter()
        .find(|m| m.sources.iter().any(|s| s.path.ends_with("first.c")))
        .unwrap();
    let destination = f.root.path().join("selected");
    let unused_config = f.root.path().join("unused.toml");
    fs::write(&unused_config,"llvm_link_filepath = '/must-not-run/llvm-link'\nllvm_ar_filepath = '/must-not-run/llvm-ar'\n").unwrap();
    let output = f
        .command("get")
        .env("RLLVM_CONFIG", unused_config)
        .arg(&input)
        .args(["--module", &module.id, "--output-dir"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let relocated = f.root.path().join("relocated");
    fs::rename(&destination, &relocated).unwrap();
    let copied = rllvm::catalog::read_catalog(&relocated.join("catalog.json")).unwrap();
    assert_eq!(copied.modules.len(), 1);
    assert_eq!(copied.modules[0].id, module.id);
    assert!(copied.modules[0].path.as_ref().unwrap().is_relative());
    let merged = f.root.path().join("selected.bc");
    let output = f
        .command("get")
        .arg(relocated.join("catalog.json"))
        .arg("-o")
        .arg(&merged)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ir = f.ir(&merged);
    assert!(ir.contains("@first"));
    assert!(!ir.contains("@second"));
}

#[test]
fn known_configuration_selection_keeps_same_source_variants_distinct() {
    let f = Fixture::new();
    let a = f.module("first", "same.c", "int value(void){return 1;}");
    let b = f.module("second", "same.c", "int value(void){return 2;}");
    let input = f.object(&[a, b]);
    let mut catalog = Fixture::catalog(
        &f.command("info")
            .arg(&input)
            .arg("--json")
            .output()
            .unwrap(),
    );
    assert_eq!(catalog.modules.len(), 2);
    assert_ne!(catalog.modules[0].id, catalog.modules[1].id);
    for (index, module) in catalog.modules.iter_mut().enumerate() {
        module.configuration_id = Some(format!("config-{index}"));
    }
    let recorded = f.root.path().join("recorded.json");
    write_catalog(&recorded, &catalog).unwrap();
    let output = f
        .command("info")
        .arg(&recorded)
        .args(["--json", "--configuration", "config-1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(Fixture::catalog(&output).modules.len(), 1);
    let output = f
        .command("info")
        .arg(&input)
        .args(["--json", "--configuration", "config-1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unmatched"));
}

#[test]
fn bitcode_archive_inventory_and_copy_preserve_member_modules() {
    let f = Fixture::new();
    let a = f.module("first", "first.c", "int first(void){return 1;}");
    let b = f.module("second", "second.c", "int second(void){return 2;}");
    let archive = f.root.path().join("modules.bca");
    assert!(
        Command::new(f.tools.join("llvm-ar"))
            .args(["--format=gnu", "rcs"])
            .arg(&archive)
            .args([a, b])
            .status()
            .unwrap()
            .success()
    );
    let result = f
        .command("info")
        .arg(&archive)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let catalog = Fixture::catalog(&result);
    assert_eq!(catalog.modules.len(), 2);
    assert!(catalog.modules.iter().all(|m| m.archive_member.is_some()));
    let destination = f.root.path().join("members");
    assert!(
        f.command("get")
            .arg(&archive)
            .arg("--output-dir")
            .arg(&destination)
            .status()
            .unwrap()
            .success()
    );
    let copied = rllvm::catalog::read_catalog(&destination.join("catalog.json")).unwrap();
    assert!(copied.modules.iter().all(|m| m.archive_member.is_none()));
    for module in copied.modules {
        assert!(
            f.ir(&destination.join(module.path.unwrap()))
                .contains("define")
        );
    }
}

#[test]
fn selected_partial_merge_preserves_original_directory_groups() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let a = f.module("first", "first.c", "int first(void){return 1;}");
    let b = f.module("second", "second.c", "int second(void){return 2;}");
    let other = f.root.path().join("other");
    fs::create_dir(&other).unwrap();
    let moved = other.join("second.bc");
    fs::rename(b, &moved).unwrap();
    let input = f.object(&[a, moved]);
    let catalog = Fixture::catalog(
        &f.command("info")
            .arg(&input)
            .arg("--json")
            .output()
            .unwrap(),
    );
    let quote = |p: PathBuf| format!("'{}'", p.display().to_string().replace('\'', "'\"'\"'"));
    let log = f.root.path().join("link.log");
    let shim = f.root.path().join("linker");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf 'link\\n' >> {}\nexec {} \"$@\"\n",
            quote(log.clone()),
            quote(f.tools.join("llvm-link"))
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let config = fs::read_to_string(&f.config).unwrap().replace(
        &f.tools.join("llvm-link").display().to_string(),
        &shim.display().to_string(),
    );
    fs::write(&f.config, config).unwrap();
    let output = f.root.path().join("partial.bc");
    let result = f
        .command("get")
        .arg(&input)
        .args([
            "--module",
            &catalog.modules[0].id,
            "--module",
            &catalog.modules[1].id,
            "--merge-strategy",
            "partial",
            "-o",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 3);
    let ir = f.ir(&output);
    assert!(ir.contains("@first") && ir.contains("@second"));
}

#[test]
fn manifest_destination_cannot_overwrite_the_input_catalog() {
    let f = Fixture::new();
    let module = f.module("first", "first.c", "int first(void){return 1;}");
    let catalog = Fixture::catalog(
        &f.command("info")
            .arg(&module)
            .arg("--json")
            .output()
            .unwrap(),
    );
    let input = f.root.path().join("result.bc.manifest");
    write_catalog(&input, &catalog).unwrap();
    let before = fs::read(&input).unwrap();
    let output = f.root.path().join("result.bc");
    let result = f
        .command("get")
        .arg(&input)
        .arg("-m")
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&input).unwrap(), before);
    assert!(!output.exists());
}
