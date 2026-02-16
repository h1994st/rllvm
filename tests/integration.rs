use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Returns the path to a compiled binary from the cargo target directory.
fn cargo_bin(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_rllvm-cc"));
    path.pop();
    path.push(name);
    path
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
    let output = Command::new(&llvm_config)
        .arg("--bindir")
        .output()
        .ok()?;
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
        if let Ok(output) = Command::new("brew").arg("--cellar").output() {
            if output.status.success() {
                let cellar = String::from_utf8_lossy(&output.stdout).trim().to_string();
                for pattern in &[
                    format!("{cellar}/llvm@*/*/bin/llvm-config"),
                    format!("{cellar}/llvm/*/bin/llvm-config"),
                ] {
                    if let Ok(paths) = glob::glob(pattern) {
                        if let Some(Ok(p)) = paths.last() {
                            return Some(p);
                        }
                    }
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
    let output = Command::new("which")
        .arg(name)
        .output()
        .map_err(|_| ())?;
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
    let status = Command::new(cargo_bin("rllvm-cc"))
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc compile failed");
    assert!(object_path.exists(), "Object file not created");

    // Step 2: Extract bitcode with rllvm-get-bc
    let bitcode_path = tmp.path().join("foo.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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

    // Compile and link foo.c + bar.c + baz.c into a single output
    let status = Command::new(cargo_bin("rllvm-cc"))
        .args(["--", "-o"])
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
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
    let status = Command::new(cargo_bin("rllvm-cc"))
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(&c_source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc compile failed");

    // Extract bitcode
    let bitcode_path = tmp.path().join("main.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
    let status = Command::new(cargo_bin("rllvm-cc"))
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
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
    let status = Command::new(cargo_bin("rllvm-cxx"))
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
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
    let status = Command::new(cargo_bin("rllvm-cc"))
        .args(["--", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success());

    // Extract with manifest flag
    let bitcode_path = tmp.path().join("foo.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
    let status = Command::new(cargo_bin("rllvm-cc"))
        .args(["--", "-O2", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc -O2 compile failed");

    // Extract and verify bitcode
    let bitcode_path = tmp.path().join("foo_opt.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
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
            let status = Command::new(cargo_bin("rllvm-cc"))
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
    let status = Command::new(cargo_bin("rllvm-get-bc"))
        .arg(&archive_path)
        .args(["-o"])
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        status.success(),
        "rllvm-get-bc failed on static archive"
    );

    assert_valid_bitcode(&bitcode_path);
}
