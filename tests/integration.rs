use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::TempDir;

/// Returns the path to a compiled binary from the cargo target directory.
fn cargo_bin(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_rllvm-cc"));
    path.pop();
    path.push(name);
    path
}

/// Writes a configuration file describing the LLVM installation on this machine
/// and returns its path.
///
/// Without this the wrappers fall back to `~/.rllvm/config.toml`, which makes
/// the suite depend on whatever the developer happens to have configured — a
/// stale entry there fails every test before any behavior is exercised.
fn test_config_path() -> &'static Path {
    static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();
    CONFIG_PATH.get_or_init(|| {
        let llvm_config = find_llvm_config().expect("llvm-config not found; is LLVM installed?");
        let output = Command::new(&llvm_config)
            .arg("--bindir")
            .output()
            .expect("Failed to run llvm-config --bindir");
        assert!(output.status.success(), "llvm-config --bindir failed");
        let bindir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

        let tool = |name: &str| bindir.join(name).display().to_string();
        let contents = format!(
            "llvm_config_filepath = '{}'\n\
             clang_filepath = '{}'\n\
             clangxx_filepath = '{}'\n\
             llvm_ar_filepath = '{}'\n\
             llvm_link_filepath = '{}'\n\
             llvm_objcopy_filepath = '{}'\n",
            llvm_config.display(),
            tool("clang"),
            tool("clang++"),
            tool("llvm-ar"),
            tool("llvm-link"),
            tool("llvm-objcopy"),
        );

        let config_dir = std::env::temp_dir().join("rllvm-integration-tests");
        fs::create_dir_all(&config_dir).expect("Failed to create the test config directory");
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, contents).expect("Failed to write the test config file");
        config_path
    })
}

/// Returns a `Command` for one of the rllvm binaries, pinned to the isolated
/// test configuration.
fn rllvm(name: &str) -> Command {
    let mut command = Command::new(cargo_bin(name));
    command.env("RLLVM_CONFIG", test_config_path());
    command
}

/// Returns the absolute path to a test fixture file in tests/data/.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

/// Finds llvm-dis on the system for bitcode validation.
fn find_llvm_dis() -> Option<PathBuf> {
    // Try to get the LLVM bin directory from llvm-config
    let llvm_config = find_llvm_config()?;
    let output = Command::new(&llvm_config).arg("--bindir").output().ok()?;
    if output.status.success() {
        let bindir = String::from_utf8(output.stdout).ok()?.trim().to_string();
        let llvm_dis = PathBuf::from(&bindir).join("llvm-dis");
        if llvm_dis.exists() {
            return Some(llvm_dis);
        }
    }
    None
}

/// Finds llvm-config on the system (mirrors the crate's discovery logic).
fn find_llvm_config() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("LLVM_CONFIG") {
        let p = PathBuf::from(val);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(p) = which("llvm-config") {
        return Some(p);
    }
    #[cfg(target_vendor = "apple")]
    {
        // Search Homebrew cellar
        if let Ok(output) = Command::new("brew").arg("--cellar").output()
            && output.status.success()
        {
            let cellar = String::from_utf8_lossy(&output.stdout).trim().to_string();
            for pattern in &[
                format!("{cellar}/llvm@*/*/bin/llvm-config"),
                format!("{cellar}/llvm/*/bin/llvm-config"),
            ] {
                if let Ok(paths) = glob::glob(pattern)
                    && let Some(Ok(p)) = paths.last()
                {
                    return Some(p);
                }
            }
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        for v in (6..=33).rev() {
            if let Ok(p) = which(&format!("llvm-config-{v}")) {
                return Some(p);
            }
        }
    }
    None
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let output = Command::new("which").arg(name).output().map_err(|_| ())?;
    if output.status.success() {
        let path = String::from_utf8(output.stdout)
            .map_err(|_| ())?
            .trim()
            .to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    Err(())
}

/// Verify that a file is valid LLVM bitcode by running llvm-dis on it.
fn assert_valid_bitcode(bitcode_path: &Path) {
    assert!(
        bitcode_path.exists(),
        "Bitcode file does not exist: {bitcode_path:?}"
    );

    let llvm_dis = find_llvm_dis().expect("llvm-dis not found; is LLVM installed?");
    let output = Command::new(&llvm_dis)
        .arg(bitcode_path)
        .arg("-o")
        .arg("/dev/null")
        .output()
        .expect("Failed to run llvm-dis");

    assert!(
        output.status.success(),
        "llvm-dis failed on {bitcode_path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn compile_single_c_file_and_extract_bitcode() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");

    // Step 1: Compile foo.c with rllvm-cc (compile-only mode)
    let status = rllvm("rllvm-cc")
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc compile failed");
    assert!(object_path.exists(), "Object file not created");

    // Step 2: Extract bitcode with rllvm-get-bc
    let bitcode_path = tmp.path().join("foo.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed");

    // Step 3: Verify the extracted bitcode is valid LLVM IR
    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn compile_multiple_c_files_and_link() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("combined");

    // Compile and link foo.c + bar.c + baz.c into a single output. These
    // fixtures are library functions with no `main`, so they link as a shared
    // library rather than an executable.
    let status = rllvm("rllvm-cc")
        .args(["--", "-shared", "-fPIC", "-o"])
        .arg(&output_path)
        .arg(fixture("foo.c"))
        .arg(fixture("bar.c"))
        .arg(fixture("baz.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc link failed");
    assert!(output_path.exists(), "Linked output not created");

    // Extract bitcode from the linked output
    let bitcode_path = tmp.path().join("combined.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&output_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on linked output");

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn compile_inline_c_program() {
    let tmp = TempDir::new().unwrap();

    // Write a small C program to a temp file
    let c_source = tmp.path().join("main.c");
    std::fs::write(
        &c_source,
        r#"
int square(int x) { return x * x; }
int main(void) { return square(3) - 9; }
"#,
    )
    .unwrap();

    let object_path = tmp.path().join("main.o");

    // Compile with rllvm-cc
    let status = rllvm("rllvm-cc")
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(&c_source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc compile failed");

    // Extract bitcode
    let bitcode_path = tmp.path().join("main.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed");

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn compile_and_link_inline_c_program_to_executable() {
    let tmp = TempDir::new().unwrap();

    let c_source = tmp.path().join("hello.c");
    std::fs::write(
        &c_source,
        r#"
#include <stdio.h>
int main(void) {
    printf("hello from rllvm\n");
    return 0;
}
"#,
    )
    .unwrap();

    let exe_path = tmp.path().join("hello");

    // Compile + link with rllvm-cc
    let status = rllvm("rllvm-cc")
        .args(["--", "-o"])
        .arg(&exe_path)
        .arg(&c_source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc compile+link failed");
    assert!(exe_path.exists(), "Executable not created");

    // Verify the executable actually runs
    let output = Command::new(&exe_path)
        .output()
        .expect("Failed to run compiled executable");
    assert!(output.status.success(), "Compiled executable failed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello from rllvm"
    );

    // Extract bitcode
    let bitcode_path = tmp.path().join("hello.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&exe_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed");

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn compile_cxx_file_and_extract_bitcode() {
    let tmp = TempDir::new().unwrap();
    let exe_path = tmp.path().join("hello_cxx");

    // Compile hello.cc with rllvm-cxx
    let status = rllvm("rllvm-cxx")
        .args(["--", "-o"])
        .arg(&exe_path)
        .arg(fixture("hello.cc"))
        .status()
        .expect("Failed to run rllvm-cxx");
    assert!(status.success(), "rllvm-cxx compile+link failed");
    assert!(exe_path.exists(), "C++ executable not created");

    // Verify it runs
    let output = Command::new(&exe_path)
        .output()
        .expect("Failed to run C++ executable");
    assert!(output.status.success(), "C++ executable failed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Hello World!"
    );

    // Extract bitcode
    let bitcode_path = tmp.path().join("hello_cxx.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&exe_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on C++ output");

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn get_bc_with_manifest_flag() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");

    // Compile
    let status = rllvm("rllvm-cc")
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success());

    // Extract with manifest flag
    let bitcode_path = tmp.path().join("foo.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .args(["-m", "-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc -m failed");

    assert_valid_bitcode(&bitcode_path);

    // Check that the manifest file was created
    let manifest_path = tmp.path().join("foo.bc.manifest");
    assert!(
        manifest_path.exists(),
        "Manifest file not created at {manifest_path:?}"
    );

    let manifest_content = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(
        !manifest_content.trim().is_empty(),
        "Manifest file is empty"
    );
    // Each line in the manifest should be a path to a .bc file
    for line in manifest_content.lines() {
        assert!(
            line.ends_with(".bc"),
            "Manifest line does not end with .bc: {line}"
        );
    }
}

#[test]
fn compile_with_optimization_flags() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo_opt.o");

    // Compile with -O2
    let status = rllvm("rllvm-cc")
        .args(["--", "-O2", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc -O2 compile failed");

    // Extract and verify bitcode
    let bitcode_path = tmp.path().join("foo_opt.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success());

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn compile_to_static_archive_and_extract() {
    let tmp = TempDir::new().unwrap();

    // Compile individual object files
    let objects: Vec<PathBuf> = ["foo", "bar", "baz"]
        .iter()
        .map(|name| {
            let obj = tmp.path().join(format!("{name}.o"));
            let status = rllvm("rllvm-cc")
                .args(["--", "-c", "-o"])
                .arg(&obj)
                .arg(fixture(&format!("{name}.c")))
                .status()
                .expect("Failed to run rllvm-cc");
            assert!(status.success(), "rllvm-cc compile of {name}.c failed");
            obj
        })
        .collect();

    // Create static archive using ar
    let archive_path = tmp.path().join("libfoo.a");
    let mut ar_cmd = Command::new("ar");
    ar_cmd.arg("rcs").arg(&archive_path);
    for obj in &objects {
        ar_cmd.arg(obj);
    }
    let status = ar_cmd.status().expect("Failed to run ar");
    assert!(status.success(), "ar failed");
    assert!(archive_path.exists(), "Archive not created");

    // Extract bitcode from archive
    let bitcode_path = tmp.path().join("libfoo.a.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&archive_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on static archive");

    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn diagnostics_go_to_stderr_not_stdout() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("quiet.c");
    fs::write(&src_path, "int main(void) { return 0; }\n").unwrap();
    let output_path = tmp.path().join("quiet");

    // `-vvv` forces a high log level, so the wrapper is guaranteed to emit
    // diagnostics. They must not land on stdout: these wrappers stand in for a
    // compiler, and build systems capture stdout as real output (`-E`
    // preprocessing, `-print-*` queries), where a log line corrupts the result.
    let output = rllvm("rllvm-cc")
        .arg("--rllvm-verbose=3")
        .args(["--", "-o"])
        .arg(&output_path)
        .arg(&src_path)
        .output()
        .expect("Failed to run rllvm-cc");

    assert!(output.status.success(), "rllvm-cc failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "diagnostics leaked onto stdout: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Compiling"),
        "expected diagnostics on stderr, got: {stderr}"
    );
}

#[test]
fn compile_objective_c_file_and_extract_bitcode() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("objc.m");
    // Plain C content in a `.m` file: enough to exercise the Objective-C
    // source extension without needing an Objective-C runtime or frameworks,
    // so this runs on Linux as well as macOS.
    fs::write(&src_path, "int answer(void) { return 42; }\n").unwrap();
    let object_path = tmp.path().join("objc.o");

    let status = rllvm("rllvm-cc")
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(&src_path)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc failed on an Objective-C source");
    assert!(object_path.exists(), "Object file not created");

    // The point of the test: `.m` must be treated as a compilation input, so
    // bitcode is generated and embedded rather than silently skipped.
    let bitcode_path = tmp.path().join("objc.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed");

    assert_valid_bitcode(&bitcode_path);
}

/// `rllvm-init` must write where `RLLVM_CONFIG` points.
///
/// The path is chosen in two places — `RLLVMConfig::new()` for reading and
/// `rllvm-init` for writing — and they disagreed: init hardcoded
/// `~/.rllvm/config.toml`, so it could report writing a config that the rest of
/// the toolchain would never read, and silently overwrite the real one.
///
/// `HOME` is redirected as well, so a regression writes into the temp directory
/// rather than the developer's own `~/.rllvm/config.toml`.
#[test]
fn test_rllvm_init_honours_rllvm_config_env() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let fake_home = tmp.path().join("home");
    fs::create_dir_all(&fake_home).expect("Failed to create fake home");
    let target = tmp.path().join("nested").join("rllvm.toml");

    let output = Command::new(cargo_bin("rllvm-init"))
        .env("RLLVM_CONFIG", &target)
        .env("HOME", &fake_home)
        .output()
        .expect("Failed to run rllvm-init");
    assert!(
        output.status.success(),
        "rllvm-init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        target.exists(),
        "rllvm-init ignored RLLVM_CONFIG; nothing at {}",
        target.display()
    );
    assert!(
        !fake_home.join(".rllvm").join("config.toml").exists(),
        "rllvm-init fell back to HOME despite RLLVM_CONFIG being set"
    );
}

/// An explicit `-o` still wins over `RLLVM_CONFIG`.
#[test]
fn test_rllvm_init_output_flag_overrides_env() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let fake_home = tmp.path().join("home");
    fs::create_dir_all(&fake_home).expect("Failed to create fake home");
    let from_env = tmp.path().join("from_env.toml");
    let from_flag = tmp.path().join("from_flag.toml");

    let output = Command::new(cargo_bin("rllvm-init"))
        .arg("-o")
        .arg(&from_flag)
        .env("RLLVM_CONFIG", &from_env)
        .env("HOME", &fake_home)
        .output()
        .expect("Failed to run rllvm-init");
    assert!(output.status.success(), "rllvm-init failed");

    assert!(from_flag.exists(), "-o was not honoured");
    assert!(!from_env.exists(), "RLLVM_CONFIG overrode an explicit -o");
}

/// `rllvm-cc` must work as a drop-in `CC`, with no `--` separator.
///
/// This is the primary way a compiler wrapper is used — `CC=rllvm-cc ./configure`
/// — and gllvm's `gclang` supports it. Requiring a separator forces every user to
/// write a shim script.
#[test]
fn dropin_compile_without_separator() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");

    let output = rllvm("rllvm-cc")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "rllvm-cc rejected a bare compiler invocation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(object_path.exists(), "Object file not created");

    // The bitcode contract must still hold on this path.
    let bitcode_path = tmp.path().join("foo.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .arg("-o")
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed");
    assert_valid_bitcode(&bitcode_path);
}

/// `--version` must reach the compiler, not be answered by the wrapper.
///
/// CMake and autoconf identify the compiler by running `$CC --version`. If clap
/// answers it, they misidentify the toolchain — a failure that looks nothing
/// like an argument-parsing bug.
#[test]
fn dropin_version_passes_through_to_compiler() {
    let output = rllvm("rllvm-cc")
        .arg("--version")
        .output()
        .expect("Failed to run rllvm-cc --version");
    assert!(output.status.success(), "rllvm-cc --version failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("clang"),
        "--version did not reach the compiler; got: {stdout}"
    );
    assert!(
        !stdout.contains("rllvm-cc"),
        "the wrapper answered --version instead of the compiler; got: {stdout}"
    );
}

/// `-v` belongs to the compiler too, for the same reason as `--version`.
#[test]
fn dropin_dash_v_passes_through_to_compiler() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");

    let output = rllvm("rllvm-cc")
        .args(["-v", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "-v was not passed through: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(object_path.exists(), "Object file not created with -v");
}

/// The per-invocation compiler override survives, under its namespaced name.
#[test]
fn rllvm_compiler_override_still_works() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");
    let clang = find_llvm_config()
        .map(|c| c.parent().unwrap().join("clang"))
        .expect("llvm-config not found");

    let output = rllvm("rllvm-cc")
        .arg(format!("--rllvm-compiler={}", clang.display()))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "--rllvm-compiler was rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(object_path.exists(), "Object file not created");
}

/// Bare `--rllvm-verbose` must not consume the following compiler argument.
///
/// Without `require_equals`, clap would treat `-c` as the verbose level and the
/// compile would lose its `-c`, silently turning a compile into a link.
#[test]
fn rllvm_verbose_does_not_swallow_next_argument() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");

    let output = rllvm("rllvm-cc")
        .arg("--rllvm-verbose")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "bare --rllvm-verbose broke the invocation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        object_path.exists(),
        "-c was consumed as the verbose value; no object produced"
    );
}
