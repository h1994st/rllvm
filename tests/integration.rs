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
fn shared_config_path() -> &'static Path {
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

/// Asserts a file is LLVM bitcode by its magic bytes only.
///
/// `assert_valid_bitcode` shells out to `llvm-dis`, which fails when the
/// producer is newer than the reader. rustc bundles its own LLVM, so its
/// bitcode can be unreadable by the system tools -- CI hit exactly this:
/// "Unknown attribute kind (105) (Producer: 'LLVM22.1.8-rust', Reader: 'LLVM 18.1.3')".
/// The magic bytes are stable across versions.
fn assert_bitcode_magic(path: &Path) {
    let data = fs::read(path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"));
    assert!(data.len() >= 4, "{path:?} is too short to be bitcode");
    let head = [data[0], data[1], data[2], data[3]];
    let raw = head == [0x42, 0x43, 0xC0, 0xDE];
    let wrapped = u32::from_le_bytes(head) == 0x0B17_C0DE;
    assert!(raw || wrapped, "{path:?} is not LLVM bitcode: {head:02x?}");
}

/// Returns a `Command` for one of the rllvm binaries, pinned to the isolated
/// test configuration.
fn rllvm(name: &str) -> Command {
    let mut command = Command::new(cargo_bin(name));
    command.env("RLLVM_CONFIG", shared_config_path());
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

/// Finds llvm-nm in the same LLVM installation the wrappers use.
fn find_llvm_nm() -> Option<PathBuf> {
    let llvm_config = find_llvm_config()?;
    let output = Command::new(&llvm_config).arg("--bindir").output().ok()?;
    if output.status.success() {
        let bindir = String::from_utf8(output.stdout).ok()?.trim().to_string();
        let llvm_nm = PathBuf::from(&bindir).join("llvm-nm");
        if llvm_nm.exists() {
            return Some(llvm_nm);
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

/// Both compilations must finish before extraction: extracting the first early
/// would hide a later compilation overwriting its embedded bitcode path.
enum CompilationVariant {
    Output,
    OutputOnly,
    StoreOutputOnly,
    SharedStore,
    ReusedOutput,
    ConfiguredFlags,
    Compiler,
}

fn assert_compilation_variants_keep_bitcode(variant: CompilationVariant) {
    let shared_store = matches!(
        variant,
        CompilationVariant::SharedStore | CompilationVariant::StoreOutputOnly
    );
    let output_only = matches!(
        variant,
        CompilationVariant::OutputOnly | CompilationVariant::StoreOutputOnly
    );
    let reuse_output = matches!(
        variant,
        CompilationVariant::ReusedOutput
            | CompilationVariant::ConfiguredFlags
            | CompilationVariant::Compiler
    );
    let config_flags = matches!(variant, CompilationVariant::ConfiguredFlags);
    let compiler_variant = matches!(variant, CompilationVariant::Compiler);
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("f.c");
    fs::write(&source, if compiler_variant {
        "#ifdef __cplusplus\n#define VALUE 2\n#else\n#define VALUE 1\n#endif\nint value(void) { return VALUE; }\n"
    } else {
        "int value(void) { return VALUE; }\n"
    }).unwrap();
    let store = tmp.path().join("store");
    fs::create_dir(&store).unwrap();
    let mut config = fs::read_to_string(shared_config_path()).unwrap();
    if shared_store {
        config.push_str(&format!("bitcode_store_path = '{}'\n", store.display()));
    }
    let config_path = tmp.path().join("config.toml");
    let mut objects = Vec::new();
    for (index, name) in ["one", "two"].iter().enumerate() {
        let value = if output_only { 1 } else { index + 1 };
        let output_dir = if shared_store {
            tmp.path().join(name)
        } else {
            tmp.path().to_path_buf()
        };
        fs::create_dir_all(&output_dir).unwrap();
        let object = output_dir.join(if reuse_output {
            "same.o".into()
        } else {
            format!("{name}.o")
        });
        let definition = format!("-DVALUE={value}");
        let mut command = rllvm("rllvm-cc");
        let contents = if config_flags {
            // The machine-code invocation also needs VALUE, but only the
            // configured bitcode-generation flags vary between compilations.
            command.arg("-DVALUE=0");
            format!("{config}bitcode_generation_flags = ['-UVALUE', '{definition}']\n")
        } else {
            if compiler_variant {
                let bindir = find_llvm_dis().unwrap().parent().unwrap().to_path_buf();
                command
                    .arg("--rllvm-compiler")
                    .arg(bindir.join(if index == 0 { "clang" } else { "clang++" }));
            } else {
                command.arg(&definition);
            }
            config.clone()
        };
        fs::write(&config_path, contents).unwrap();
        let output = command
            .current_dir(tmp.path())
            .env("RLLVM_CONFIG", &config_path)
            .env("RLLVM_CACHE", "0")
            .env_remove("RLLVM_BITCODE_ROOT")
            .env_remove("RLLVM_LTO_MODE")
            .args(["-c", "-o"])
            .arg(&object)
            .arg(&source)
            .output()
            .expect("Failed to compile variant");
        assert!(
            output.status.success(),
            "variant compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Capture the first object before reusing its requested output. Its
        // bitcode reference must remain valid after the next compilation.
        let captured = tmp.path().join(format!("captured-{name}.o"));
        fs::copy(object, &captured).unwrap();
        objects.push(captured);
    }

    if output_only {
        let paths: Vec<_> = objects
            .iter()
            .map(|object| rllvm::utils::extract_bitcode_filepaths_from_object_file(object).unwrap())
            .collect();
        assert_ne!(
            paths[0], paths[1],
            "distinct requested outputs share an artifact"
        );
    }
    for (index, object) in objects.iter().enumerate() {
        let bitcode = tmp.path().join(format!("extracted-{index}.bc"));
        let output = rllvm("rllvm-get-bc")
            .env("RLLVM_CONFIG", &config_path)
            .env_remove("RLLVM_BITCODE_ROOT")
            .arg(object)
            .arg("-o")
            .arg(&bitcode)
            .output()
            .expect("Failed to extract variant");
        assert!(
            output.status.success(),
            "variant extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(find_llvm_dis().expect("llvm-dis not found"))
            .arg(&bitcode)
            .args(["-o", "-"])
            .output()
            .expect("Failed to disassemble variant");
        assert!(
            output.status.success(),
            "llvm-dis failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ir = String::from_utf8_lossy(&output.stdout);
        let expected = format!("ret i32 {}", if output_only { 1 } else { index + 1 });
        assert!(
            ir.contains(&expected),
            "variant {index} lost its original bitcode; expected {expected}:\n{ir}"
        );
    }
}

#[test]
fn compilation_variants_with_identical_flags_have_distinct_output_artifacts() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::OutputOnly);
}

#[test]
fn compilation_variants_with_identical_flags_have_distinct_store_artifacts() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::StoreOutputOnly);
}

#[test]
fn compilation_variants_in_one_directory_keep_bitcode() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::Output);
}

#[test]
fn compilation_variants_in_shared_store_keep_bitcode() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::SharedStore);
}

#[test]
fn compilation_variants_reusing_output_keep_captured_bitcode() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::ReusedOutput);
}

#[test]
fn compilation_variants_with_configured_flags_keep_captured_bitcode() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::ConfiguredFlags);
}

#[test]
fn compilation_variants_with_compiler_override_keep_captured_bitcode() {
    assert_compilation_variants_keep_bitcode(CompilationVariant::Compiler);
}

#[test]
fn joined_output_compiles_and_extracts_like_separate_output() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("f.c"), "int value(void) { return 1; }\n").unwrap();

    let joined = rllvm("rllvm-cc")
        .args(["--", "-c", "f.c", "-ojoined.o"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to run rllvm-cc with joined output");
    assert!(joined.success(), "rllvm-cc joined-output compile failed");

    let joined_extract = rllvm("rllvm-get-bc")
        .args(["joined.o", "-o", "joined.bc"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to extract joined-output bitcode");
    assert!(joined_extract.success(), "joined-output extraction failed");
    assert_valid_bitcode(&tmp.path().join("joined.bc"));

    let separate = rllvm("rllvm-cc")
        .args(["--", "-c", "f.c", "-o", "separate.o"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to run rllvm-cc with separate output");
    assert!(
        separate.success(),
        "rllvm-cc separate-output compile failed"
    );

    let separate_extract = rllvm("rllvm-get-bc")
        .args(["separate.o", "-o", "separate.bc"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to extract separate-output bitcode");
    assert!(
        separate_extract.success(),
        "separate-output extraction failed"
    );
    assert_valid_bitcode(&tmp.path().join("separate.bc"));

    assert_eq!(
        fs::read(tmp.path().join("joined.bc")).unwrap(),
        fs::read(tmp.path().join("separate.bc")).unwrap(),
        "joined and separate output forms produced different bitcode"
    );
}

#[test]
fn joined_output_names_linked_executable_and_keeps_it_extractable() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("main.c"), "int main(void) { return 0; }\n").unwrap();

    let linked = rllvm("rllvm-cc")
        .args(["--", "main.c", "-ojoined"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to run rllvm-cc with joined link output");
    assert!(linked.success(), "rllvm-cc joined-output link failed");

    let executable = tmp.path().join("joined");
    assert!(executable.exists(), "joined-output executable not created");
    assert!(
        Command::new(&executable).status().unwrap().success(),
        "joined-output executable failed"
    );

    let extracted = rllvm("rllvm-get-bc")
        .args(["joined", "-o", "joined.bc"])
        .current_dir(tmp.path())
        .status()
        .expect("Failed to extract joined link output");
    assert!(extracted.success(), "joined link output extraction failed");
    assert_valid_bitcode(&tmp.path().join("joined.bc"));
}

#[test]
fn separate_object_file_name_keeps_default_object_extractable() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("f.c"), "int value(void) { return 1; }\n").unwrap();

    let output = rllvm("rllvm-cc")
        .args(["--", "-c", "f.c", "-object-file-name", "debug.o"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run rllvm-cc with separate -object-file-name");
    assert!(
        output.status.success(),
        "rllvm-cc with separate -object-file-name failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let extract = rllvm("rllvm-get-bc")
        .args(["f.o", "-o", "f.bc"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to extract bitcode from the default object");
    assert!(
        extract.status.success(),
        "default-object extraction failed: {}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_valid_bitcode(&tmp.path().join("f.bc"));
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

fn assert_pthread_compilation(compile_only: bool) {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("threaded.c");
    fs::write(
        &source,
        "int main(void) {\n#ifdef _REENTRANT\nreturn 0;\n#else\nreturn 23;\n#endif\n}\n",
    )
    .unwrap();
    let program = tmp.path().join("threaded");
    let artifact = if compile_only {
        tmp.path().join("threaded.o")
    } else {
        program.clone()
    };
    let mut compile = rllvm("rllvm-cc");
    compile.env("RLLVM_CACHE", "0").args(["-pthread", "-O1"]);
    if compile_only {
        compile.arg("-c");
    }
    let output = compile
        .arg(&source)
        .arg("-o")
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "pthread compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if compile_only {
        let status = rllvm("rllvm-cc")
            .arg("-pthread")
            .arg(&artifact)
            .arg("-o")
            .arg(&program)
            .status()
            .unwrap();
        assert!(status.success());
    }
    assert_eq!(Command::new(&program).status().unwrap().code(), Some(0));

    let bitcode = tmp.path().join("extracted.bc");
    assert!(
        rllvm("rllvm-get-bc")
            .arg(&artifact)
            .arg("-o")
            .arg(&bitcode)
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(find_llvm_dis().unwrap())
        .arg(&bitcode)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(ir.contains("ret i32 0"), "bitcode lost _REENTRANT: {ir}");
}

#[test]
fn pthread_preserves_reentrant_in_object_and_bitcode() {
    assert_pthread_compilation(true);
}

#[test]
fn pthread_preserves_reentrant_in_linked_program_and_bitcode() {
    assert_pthread_compilation(false);
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
fn rllvm_init_honours_rllvm_config_env() {
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
fn rllvm_init_output_flag_overrides_env() {
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

/// A build tree recorded with `RLLVM_BITCODE_ROOT` survives being moved.
///
/// Absolute paths pin an object to the directory that built it. This breaks
/// under `mv`, when artifacts are copied out of a container, when a compiler
/// cache replays an object into a different tree, and when CI hands objects to
/// another job. Recording relative to a root and supplying the root again at
/// extraction time makes the object portable.
#[test]
fn relocated_build_tree_extracts_with_bitcode_root() {
    let tmp = TempDir::new().unwrap();
    let build_a = tmp.path().join("a");
    fs::create_dir_all(&build_a).unwrap();
    let src = build_a.join("foo.c");
    fs::copy(fixture("foo.c"), &src).unwrap();
    let object_path = build_a.join("foo.o");

    let status = rllvm("rllvm-cc")
        .env("RLLVM_BITCODE_ROOT", &build_a)
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&src)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "compile failed");

    // The build directory moves; the object and its bitcode travel together.
    let build_b = tmp.path().join("b");
    fs::rename(&build_a, &build_b).unwrap();

    let bitcode_path = tmp.path().join("foo.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(build_b.join("foo.o"))
        .arg("--bitcode-root")
        .arg(&build_b)
        .arg("-o")
        .arg(&bitcode_path)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "extraction failed after relocation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_valid_bitcode(&bitcode_path);
}

/// Negative control: without a root, the recorded path is absolute and the same
/// relocation breaks extraction. If this ever passes, the test above proves
/// nothing.
#[test]
fn relocated_build_tree_fails_without_bitcode_root() {
    let tmp = TempDir::new().unwrap();
    let build_a = tmp.path().join("a");
    fs::create_dir_all(&build_a).unwrap();
    let src = build_a.join("foo.c");
    fs::copy(fixture("foo.c"), &src).unwrap();
    let object_path = build_a.join("foo.o");

    // No RLLVM_BITCODE_ROOT: the historical absolute form.
    let status = rllvm("rllvm-cc")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&src)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "compile failed");

    let build_b = tmp.path().join("b");
    fs::rename(&build_a, &build_b).unwrap();

    let bitcode_path = tmp.path().join("foo.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(build_b.join("foo.o"))
        .arg("-o")
        .arg(&bitcode_path)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        !output.status.success(),
        "absolute paths unexpectedly survived relocation; the positive test is vacuous"
    );
}

/// Each wrapper must identify itself by its own name.
///
/// `rllvm-cxx` reuses `rllvm_cc.rs`, which hardcoded `name = "rllvm-cc"` in the
/// clap derive, so `rllvm-cxx --rllvm-version` reported `rllvm-cc` and its help
/// showed the wrong usage line.
#[test]
fn each_wrapper_reports_its_own_name() {
    for bin in ["rllvm-cc", "rllvm-cxx"] {
        let output = rllvm(bin)
            .arg("--rllvm-version")
            .output()
            .unwrap_or_else(|e| panic!("Failed to run {bin}: {e}"));
        assert!(output.status.success(), "{bin} --rllvm-version failed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with(bin),
            "{bin} identified itself as {stdout:?}"
        );
    }
}

/// `--rllvm-help` must describe the tool, not explain its implementation.
///
/// clap promotes a struct's doc comment to `long_about`, so the rationale for
/// the argument layout was being printed to users who asked for help.
#[test]
fn wrapper_help_does_not_leak_implementation_notes() {
    let output = rllvm("rllvm-cc")
        .arg("--rllvm-help")
        .output()
        .expect("Failed to run rllvm-cc --rllvm-help");
    assert!(output.status.success(), "--rllvm-help failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Wrapper arguments"),
        "help leaked the struct's implementation notes: {stdout}"
    );
    assert!(
        stdout.contains("Execute the wrapped clang compiler"),
        "help lost its description: {stdout}"
    );
}

/// Linking without `-o` must work: the compiler's default output is `a.out`.
///
/// `configure` probes the toolchain with bare `$CC conftest.c` and no `-o`, so
/// this is the very first thing autoconf does. `output_filename` is only set
/// when `-o` is parsed, so it stayed empty and `PathBuf::from("").canonicalize()`
/// failed with ENOENT — surfacing as "C compiler cannot create executables",
/// which looks nothing like a wrapper bug.
///
/// CMake always passes `-o`, which is why this survived a full CMake build.
#[test]
fn link_without_output_flag_defaults_to_a_out() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("conftest.c");
    fs::write(&src, "int main(void) { return 0; }\n").unwrap();

    let output = rllvm("rllvm-cc")
        .current_dir(tmp.path())
        .arg("conftest.c")
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "linking without -o failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join("a.out").exists(), "a.out was not produced");

    // The bitcode contract must hold on this path too.
    let bitcode_path = tmp.path().join("a.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(tmp.path().join("a.out"))
        .arg("-o")
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on a.out");
    assert_valid_bitcode(&bitcode_path);
}

#[test]
fn get_bc_archive_replaces_output_only_after_success() {
    let tmp = TempDir::new().unwrap();
    let program = build_across_two_directories(&tmp);
    let archive = tmp.path().join("program.bca");
    let extract = |input: &Path| {
        rllvm("rllvm-get-bc")
            .arg(input)
            .args(["--merge-strategy", "archive", "-o"])
            .arg(&archive)
            .output()
            .unwrap()
    };
    let members = |path: &Path| {
        let data = fs::read(path).unwrap();
        object::read::archive::ArchiveFile::parse(&*data)
            .unwrap()
            .members()
            .map(|member| member.unwrap().name().to_vec())
            .collect::<Vec<_>>()
    };
    let output = extract(&program);
    assert!(
        output.status.success(),
        "initial archive failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(members(&archive).len(), 2);

    let single_object = tmp.path().join("a/a.o");
    let bitcode_paths =
        rllvm::utils::extract_bitcode_filepaths_from_object_file(&single_object).unwrap();
    assert_eq!(bitcode_paths.len(), 1);
    let output = extract(&single_object);
    assert!(
        output.status.success(),
        "replacement archive failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_name = bitcode_paths[0].file_name().unwrap().as_encoded_bytes();
    assert_eq!(
        members(&archive),
        vec![expected_name.to_vec()],
        "the replacement retained modules absent from the current input"
    );

    let before_failure = fs::read(&archive).unwrap();
    fs::remove_file(&bitcode_paths[0]).unwrap();
    let output = extract(&single_object);
    assert!(
        !output.status.success(),
        "missing bitcode must fail extraction"
    );
    assert_eq!(fs::read(&archive).unwrap(), before_failure);
}

/// Compiles two sources that live in *different* directories and links them.
///
/// Returns (executable, temp dir). `partial` groups bitcode by parent directory
/// and falls back to a full link when there is only one group, so a fixture
/// spread across directories is what exercises its real path.
fn build_across_two_directories(tmp: &TempDir) -> PathBuf {
    let a_dir = tmp.path().join("a");
    let b_dir = tmp.path().join("b");
    fs::create_dir_all(&a_dir).unwrap();
    fs::create_dir_all(&b_dir).unwrap();

    fs::write(a_dir.join("a.c"), "int a_value(void) { return 1; }\n").unwrap();
    fs::write(
        b_dir.join("b.c"),
        "int a_value(void);\nint main(void) { return a_value() - 1; }\n",
    )
    .unwrap();

    let a_obj = a_dir.join("a.o");
    let b_obj = b_dir.join("b.o");
    for (src, obj) in [(a_dir.join("a.c"), &a_obj), (b_dir.join("b.c"), &b_obj)] {
        let status = rllvm("rllvm-cc")
            .args(["-c", "-o"])
            .arg(obj)
            .arg(src)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "compile failed");
    }

    let exe = tmp.path().join("prog");
    let status = rllvm("rllvm-cc")
        .arg("-o")
        .arg(&exe)
        .arg(&a_obj)
        .arg(&b_obj)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "link failed");
    exe
}

/// Every merge strategy produces a usable artifact from the same input.
#[test]
fn get_bc_merge_strategies_all_produce_output() {
    let tmp = TempDir::new().unwrap();
    let exe = build_across_two_directories(&tmp);

    for (strategy, ext) in [("full", "bc"), ("partial", "bc"), ("archive", "bca")] {
        let out = tmp.path().join(format!("{strategy}.{ext}"));
        let output = rllvm("rllvm-get-bc")
            .arg(&exe)
            .args(["--merge-strategy", strategy, "-o"])
            .arg(&out)
            .output()
            .expect("Failed to run rllvm-get-bc");
        assert!(
            output.status.success(),
            "merge-strategy={strategy} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(out.exists(), "merge-strategy={strategy} produced no output");
    }

    // full and partial are different routes to the same module, so they must
    // agree on content.
    assert_valid_bitcode(&tmp.path().join("full.bc"));
    assert_valid_bitcode(&tmp.path().join("partial.bc"));
}

/// `partial` must not use predictable sibling paths for its intermediates.
#[test]
fn get_bc_partial_preserves_existing_sibling() {
    let tmp = TempDir::new().unwrap();
    let exe = build_across_two_directories(&tmp);
    let out = tmp.path().join("merged.bc");
    let sibling = tmp.path().join("merged_partial_0.bc");
    let sentinel = b"existing file must survive\n";
    fs::write(&sibling, sentinel).unwrap();

    let output = rllvm("rllvm-get-bc")
        .arg(&exe)
        .args(["--merge-strategy", "partial", "-o"])
        .arg(&out)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "partial merge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_valid_bitcode(&out);
    assert_eq!(
        fs::read(&sibling).expect("preexisting sibling was removed"),
        sentinel
    );
}

/// A failed partial link cleans up its workspace and leaves the requested
/// output untouched when it already exists.
#[test]
fn get_bc_partial_error_cleans_owned_temps_and_preserves_output() {
    let tmp = TempDir::new().unwrap();
    let exe = build_across_two_directories(&tmp);
    let b_bitcode = fs::read_dir(tmp.path().join("b"))
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().is_some_and(|ext| ext == "bc"))
        .expect("the wrapper did not produce bitcode for b.c");
    fs::write(b_bitcode, b"not LLVM bitcode").unwrap();

    let out = tmp.path().join("merged.bc");
    let sentinel = b"existing output must survive\n";
    fs::write(&out, sentinel).unwrap();
    let sibling = tmp.path().join("merged_partial_0.bc");
    let sibling_sentinel = b"existing sibling must survive an error\n";
    fs::write(&sibling, sibling_sentinel).unwrap();

    let temp_root = tmp.path().join("merge-temporaries");
    fs::create_dir(&temp_root).unwrap();

    let output = rllvm("rllvm-get-bc")
        .env("TMPDIR", &temp_root)
        .env("TMP", &temp_root)
        .env("TEMP", &temp_root)
        .arg(&exe)
        .args(["--merge-strategy", "partial", "-o"])
        .arg(&out)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        !output.status.success(),
        "partial merge unexpectedly succeeded"
    );
    assert_eq!(fs::read(&out).unwrap(), sentinel);
    assert_eq!(
        fs::read(&sibling).expect("preexisting sibling was removed"),
        sibling_sentinel
    );
    assert!(!tmp.path().join("merged_partial_1.bc").exists());

    let temp_leftovers: Vec<_> = fs::read_dir(&temp_root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    assert!(
        temp_leftovers.is_empty(),
        "partial left its temporary workspace behind: {temp_leftovers:?}"
    );
}

/// Completions generate for every shell and every binary.
#[test]
fn completions_generate_for_all_shells_and_binaries() {
    for bin in ["cc", "cxx", "get-bc"] {
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let output = rllvm("rllvm-completions")
                .args(["--shell", shell, "--bin", bin])
                .output()
                .expect("Failed to run rllvm-completions");
            assert!(
                output.status.success(),
                "completions failed for {bin}/{shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.stdout.is_empty(),
                "completions for {bin}/{shell} were empty"
            );
        }
    }
}

/// `rllvm-info` accepts both a raw `.bc` and an object file with an embedded path.
#[test]
fn info_reports_on_bitcode_and_on_object_files() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");
    let status = rllvm("rllvm-cc")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "compile failed");

    let bitcode_path = tmp.path().join("foo.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&object_path)
        .arg("-o")
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "extraction failed");

    // Raw bitcode, and the object file it came from: both must be accepted, and
    // the object path exercises the embedded-section lookup.
    for input in [&bitcode_path, &object_path] {
        let output = rllvm("rllvm-info")
            .arg(input)
            .output()
            .expect("Failed to run rllvm-info");
        assert!(
            output.status.success(),
            "rllvm-info failed on {input:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "rllvm-info produced no output for {input:?}"
        );
    }

    // -f lists function names; foo.c defines one.
    let output = rllvm("rllvm-info")
        .arg("-f")
        .arg(&bitcode_path)
        .output()
        .expect("Failed to run rllvm-info");
    assert!(output.status.success(), "rllvm-info -f failed");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("foo"),
        "-f did not list the expected function"
    );
}

/// `llvm-dis` appends predecessor comments to explicit block labels, while the
/// entry block can be implicit.
#[test]
fn info_counts_implicit_entry_and_pred_labeled_blocks() {
    let tmp = TempDir::new().unwrap();
    let source_path = tmp.path().join("branch.c");
    let bitcode_path = tmp.path().join("branch.bc");
    fs::write(
        &source_path,
        "int value(int x) { if (x) return 1; return 2; }\n",
    )
    .unwrap();

    let clang = find_llvm_config()
        .map(|config| config.parent().unwrap().join("clang"))
        .expect("clang not found");
    let output = Command::new(clang)
        .args(["-O0", "-emit-llvm", "-c", "-o"])
        .arg(&bitcode_path)
        .arg(&source_path)
        .output()
        .expect("clang failed");
    assert!(
        output.status.success(),
        "clang failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = rllvm("rllvm-info")
        .arg(&bitcode_path)
        .output()
        .expect("Failed to run rllvm-info");
    assert!(
        output.status.success(),
        "rllvm-info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Basic blocks : 4"),
        "rllvm-info reported the wrong block count: {stdout}"
    );
}

/// A file that is neither bitcode nor an object must fail cleanly, not panic.
#[test]
fn info_rejects_a_file_that_is_neither_bitcode_nor_object() {
    let tmp = TempDir::new().unwrap();
    let junk = tmp.path().join("junk.txt");
    fs::write(&junk, b"not bitcode, not an object file\n").unwrap();

    let output = rllvm("rllvm-info")
        .arg(&junk)
        .output()
        .expect("Failed to run rllvm-info");
    assert!(!output.status.success(), "junk input unexpectedly accepted");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "rllvm-info panicked instead of reporting an error: {stderr}"
    );
}

/// `rllvm-rustc` works both as `RUSTC` and as `RUSTC_WRAPPER`.
///
/// cargo invokes a wrapper as `rllvm-rustc <path-to-rustc> <args...>` but a
/// replacement as `rllvm-rustc <args...>`, and the binary tells them apart by
/// inspecting argv[1]. Neither mode had any coverage.
#[test]
fn rustc_wrapper_handles_both_invocation_modes() {
    let rustc = which("rustc").expect("rustc not found");

    // RUSTC_WRAPPER mode: argv[1] is the real rustc.
    let output = rllvm("rllvm-rustc")
        .arg(&rustc)
        .arg("--version")
        .output()
        .expect("Failed to run rllvm-rustc in wrapper mode");
    assert!(
        output.status.success(),
        "wrapper mode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("rustc"),
        "wrapper mode did not reach rustc"
    );

    // RUSTC mode: no rustc path, so the binary must find one itself.
    let output = rllvm("rllvm-rustc")
        .arg("--version")
        .output()
        .expect("Failed to run rllvm-rustc in rustc mode");
    assert!(
        output.status.success(),
        "rustc mode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("rustc"),
        "rustc mode did not reach rustc"
    );
}

/// `RLLVM_REAL_RUSTC` overrides the discovered rustc.
#[test]
fn rustc_wrapper_honours_real_rustc_override() {
    let rustc = which("rustc").expect("rustc not found");

    let output = rllvm("rllvm-rustc")
        .env("RLLVM_REAL_RUSTC", &rustc)
        .arg("--version")
        .output()
        .expect("Failed to run rllvm-rustc");
    assert!(output.status.success(), "override was rejected");
    assert!(String::from_utf8_lossy(&output.stdout).contains("rustc"));
}

/// Writes a config identical to the test one but with a chosen key removed.
fn config_without(key: &str, dir: &Path) -> PathBuf {
    let base = fs::read_to_string(shared_config_path()).expect("read test config");
    let filtered: String = base
        .lines()
        .filter(|l| !l.trim_start().starts_with(key))
        .map(|l| format!("{l}\n"))
        .collect();
    let path = dir.join("config.toml");
    fs::write(&path, filtered).expect("write config");
    path
}

/// The embed path must work when `llvm-objcopy` is not configured.
///
/// Embedding prefers `llvm-objcopy` and falls back to rebuilding the object
/// through the `object` crate. The fallback is the lossy one, so it is the more
/// important of the two to keep working — and every other test takes the
/// objcopy path, because the test config always has it.
#[test]
fn embedding_falls_back_when_objcopy_is_unavailable() {
    let tmp = TempDir::new().unwrap();
    let cfg = config_without("llvm_objcopy_filepath", tmp.path());
    let object_path = tmp.path().join("foo.o");

    let output = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "compile failed without objcopy: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The contract must still hold on the fallback path.
    let bitcode_path = tmp.path().join("foo.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
        .env("RLLVM_CONFIG", &cfg)
        .arg(&object_path)
        .arg("-o")
        .arg(&bitcode_path)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "extraction failed on the fallback path");
    assert_valid_bitcode(&bitcode_path);
}

/// A config pointing at a non-existent objcopy also takes the fallback.
#[test]
fn embedding_falls_back_when_objcopy_path_is_missing() {
    let tmp = TempDir::new().unwrap();
    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cfg = tmp.path().join("config.toml");
    let rewritten: String = base
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("llvm_objcopy_filepath") {
                "llvm_objcopy_filepath = '/nonexistent/llvm-objcopy'".to_string()
            } else {
                l.to_string()
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&cfg, rewritten).unwrap();

    let object_path = tmp.path().join("foo.o");
    let output = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "compile failed with a bogus objcopy path: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(object_path.exists());
}

/// Exercises the object-rebuild embed path, which neither other tier reaches.
///
/// Embedding has three tiers: `llvm-objcopy`, a Mach-O in-place builder, and a
/// full rebuild through the `object` crate. The rebuild only runs for a
/// non-Mach-O object when objcopy is unavailable, so it needs both conditions
/// arranged at once — and it is the lossy tier, so leaving it untested is the
/// worst of the three.
///
/// `--target` makes clang emit ELF regardless of host, so this runs everywhere.
#[test]
fn embedding_rebuilds_elf_objects_without_objcopy() {
    let tmp = TempDir::new().unwrap();
    let cfg = config_without("llvm_objcopy_filepath", tmp.path());
    let object_path = tmp.path().join("foo.o");

    let output = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .args(["--target=x86_64-unknown-linux-gnu", "-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "ELF compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The embedded path must survive the rebuild.
    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&object_path)
        .expect("failed to read the embedded section back");
    assert_eq!(paths.len(), 1, "expected exactly one embedded bitcode path");
    assert!(
        paths[0].exists(),
        "embedded path does not exist: {:?}",
        paths[0]
    );
}

/// `rllvm-get-bc` on a path that does not exist fails cleanly.
#[test]
fn get_bc_reports_a_missing_input() {
    let tmp = TempDir::new().unwrap();
    let output = rllvm("rllvm-get-bc")
        .arg(tmp.path().join("nope.o"))
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(!output.status.success(), "missing input was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "panicked instead of failing: {stderr}"
    );
}

/// An object with no embedded bitcode section fails with a clear error.
#[test]
fn get_bc_reports_an_object_without_bitcode() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("plain.c");
    fs::write(&src, "int plain(void) { return 0; }\n").unwrap();
    let obj = tmp.path().join("plain.o");

    // Compile with the real clang, so no section is embedded.
    let clang = find_llvm_config()
        .map(|c| c.parent().unwrap().join("clang"))
        .expect("clang not found");
    let status = Command::new(clang)
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .status()
        .expect("clang failed");
    assert!(status.success());

    let output = rllvm("rllvm-get-bc")
        .arg(&obj)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        !output.status.success(),
        "an object with no bitcode was accepted"
    );
}

/// Verbosity levels are accepted and do not corrupt stdout.
#[test]
fn get_bc_verbosity_levels_are_accepted() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");
    let status = rllvm("rllvm-cc")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("compile failed");
    assert!(status.success());

    for flag in ["-v", "-vv", "-vvv", "-vvvv"] {
        let out = tmp.path().join(format!("out{}.bc", flag.len()));
        let output = rllvm("rllvm-get-bc")
            .arg(flag)
            .arg(&object_path)
            .arg("-o")
            .arg(&out)
            .output()
            .expect("Failed to run rllvm-get-bc");
        assert!(output.status.success(), "{flag} failed");
        assert!(out.exists());
    }
}

/// `-b` selects the archive strategy when `--merge-strategy` is absent.
#[test]
fn get_bc_dash_b_selects_archive() {
    let tmp = TempDir::new().unwrap();
    let object_path = tmp.path().join("foo.o");
    let status = rllvm("rllvm-cc")
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(fixture("foo.c"))
        .status()
        .expect("compile failed");
    assert!(status.success());

    let out = tmp.path().join("foo.bca");
    let status = rllvm("rllvm-get-bc")
        .arg("-b")
        .arg(&object_path)
        .arg("-o")
        .arg(&out)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "-b failed");
    assert!(out.exists(), "-b produced no archive");
}

/// `rllvm-init --dry-run` prints a config without writing one.
#[test]
fn init_dry_run_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("config.toml");

    let output = Command::new(cargo_bin("rllvm-init"))
        .arg("--dry-run")
        .arg("-o")
        .arg(&target)
        .env("HOME", tmp.path())
        .output()
        .expect("Failed to run rllvm-init");
    assert!(output.status.success(), "--dry-run failed");
    assert!(!target.exists(), "--dry-run wrote a config file");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("clang_filepath"),
        "--dry-run printed no config"
    );
}

/// `--llvm-prefix` pointing somewhere without LLVM fails cleanly.
#[test]
fn init_reports_a_bad_llvm_prefix() {
    let tmp = TempDir::new().unwrap();
    let output = Command::new(cargo_bin("rllvm-init"))
        .args(["--llvm-prefix", "/nonexistent/llvm"])
        .env("HOME", tmp.path())
        .output()
        .expect("Failed to run rllvm-init");
    assert!(!output.status.success(), "a bogus prefix was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "panicked: {stderr}");
}

/// `--llvm-prefix` accepts both a prefix and its bin directory.
#[test]
fn init_accepts_prefix_and_bindir() {
    let llvm_config = find_llvm_config().expect("llvm-config not found");
    let bindir = llvm_config.parent().unwrap().to_path_buf();
    let prefix = bindir.parent().unwrap().to_path_buf();

    for candidate in [prefix, bindir] {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("config.toml");
        let output = Command::new(cargo_bin("rllvm-init"))
            .arg("--llvm-prefix")
            .arg(&candidate)
            .arg("-o")
            .arg(&target)
            .env("HOME", tmp.path())
            .output()
            .expect("Failed to run rllvm-init");
        assert!(
            output.status.success(),
            "--llvm-prefix {candidate:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(target.exists(), "no config written for {candidate:?}");
    }
}

/// Rebuilds an ELF object whose content exercises the whole copy path.
///
/// `copy_object_file` walks sections, symbols and relocations. A trivial
/// translation unit touches almost none of that, so this fixture deliberately
/// contains a BSS global, an undefined external, a static, and references that
/// produce relocations.
#[test]
fn embedding_rebuilds_an_elf_object_with_varied_content() {
    let tmp = TempDir::new().unwrap();
    let cfg = config_without("llvm_objcopy_filepath", tmp.path());

    let src = tmp.path().join("rich.c");
    fs::write(
        &src,
        r#"
int uninitialized_global;              /* BSS */
int initialized_global = 42;           /* data */
static int static_value = 7;           /* local symbol */
extern int external_symbol;            /* undefined */
extern int external_fn(int);

int use_everything(void) {
    return external_symbol + uninitialized_global + initialized_global
         + static_value + external_fn(1);
}
"#,
    )
    .unwrap();

    let object_path = tmp.path().join("rich.o");
    let output = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .args(["--target=x86_64-unknown-linux-gnu", "-c", "-o"])
        .arg(&object_path)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "rich ELF compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&object_path)
        .expect("failed to read the embedded section back");
    assert_eq!(paths.len(), 1);

    // The rebuilt object must still parse and keep its symbols.
    let data = fs::read(&object_path).unwrap();
    let obj = object::File::parse(&*data).expect("rebuilt object does not parse");
    let names: Vec<String> = {
        use object::{Object, ObjectSymbol};
        obj.symbols()
            .filter_map(|s| s.name().ok().map(|n| n.to_string()))
            .collect()
    };
    for expected in ["use_everything", "external_symbol", "uninitialized_global"] {
        assert!(
            names.iter().any(|n| n == expected),
            "symbol {expected} lost in the rebuild; have {names:?}"
        );
    }
}

/// `bitcode_store_path` redirects bitcode into a central directory.
///
/// A documented config option with no coverage. The store also renames each
/// file using a hash of the source path, so two sources with the same stem in
/// different directories cannot collide.
#[test]
fn bitcode_store_path_collects_bitcode_centrally() {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("store");
    fs::create_dir_all(&store).unwrap();

    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cfg = tmp.path().join("config.toml");
    fs::write(
        &cfg,
        format!("{base}bitcode_store_path = '{}'\n", store.display()),
    )
    .unwrap();

    // Two sources sharing a stem, in different directories.
    let mut objects = vec![];
    for dir in ["a", "b"] {
        let d = tmp.path().join(dir);
        fs::create_dir_all(&d).unwrap();
        let src = d.join("same.c");
        fs::write(&src, format!("int {dir}_fn(void) {{ return 0; }}\n")).unwrap();
        let obj = d.join("same.o");

        let output = Command::new(cargo_bin("rllvm-cc"))
            .env("RLLVM_CONFIG", &cfg)
            .args(["-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("Failed to run rllvm-cc");
        assert!(
            output.status.success(),
            "compile failed with a bitcode store: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        objects.push(obj);
    }

    let stored: Vec<_> = fs::read_dir(&store)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        stored.len(),
        2,
        "same-stem sources collided in the store: {stored:?}"
    );

    // Extraction must still resolve through the store.
    let out = tmp.path().join("same.bc");
    let status = Command::new(cargo_bin("rllvm-get-bc"))
        .env("RLLVM_CONFIG", &cfg)
        .arg(&objects[0])
        .arg("-o")
        .arg(&out)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "extraction failed with a bitcode store");
    assert_valid_bitcode(&out);
}

/// `-dead_strip` reaches the linker and is not reported as dropped.
///
/// rllvm used to delete the flag, because its embedded section was
/// unreferenced and ld discarded it, which silently handed the user a binary
/// their command had not asked for. The section now carries
/// `S_ATTR_NO_DEAD_STRIP`, so the flag is passed through like any other.
///
/// Darwin only: `-dead_strip` is an ld64 flag, and clang rejects it elsewhere.
#[test]
#[cfg(target_os = "macos")]
fn dead_strip_is_passed_through_without_warning() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("main.c");
    fs::write(&src, "int main(void) { return 0; }\n").unwrap();
    let exe = tmp.path().join("prog");

    let output = rllvm("rllvm-cc")
        .arg("-dead_strip")
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(exe.exists(), "no executable produced");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("dead_strip"),
        "the flag is honoured now, so nothing should be reported: {stderr}"
    );

    // And the bitcode path is still recoverable from the stripped binary.
    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&exe)
        .expect("no embedded section survived the dead-stripping link");
    assert_eq!(paths.len(), 1, "expected one embedded path, got {paths:?}");
}

/// `lto_ldflags` are appended when linking with `-flto`.
#[test]
fn lto_ldflags_are_appended_when_linking_with_lto() {
    let tmp = TempDir::new().unwrap();
    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cfg = tmp.path().join("config.toml");
    fs::write(&cfg, format!("{base}lto_ldflags = ['-Wl,-v']\n")).unwrap();

    let src = tmp.path().join("main.c");
    fs::write(&src, "int main(void) { return 0; }\n").unwrap();
    let obj = tmp.path().join("main.o");

    // `lto_ldflags` reaching the link is orthogonal to `lto_mode` -- it lives
    // in `command()`/`link_object_files`, not in the skip decision. This test
    // predates `lto_mode` and was written when `-flto` always skipped bitcode
    // generation; pin to `skip` so it keeps exercising exactly that, unrelated
    // path rather than tripping over the default `marker` mode's bitcode
    // artifact, which Task 5's tests cover instead.
    let status = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .env("RLLVM_LTO_MODE", "skip")
        .args(["-flto", "-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .status()
        .expect("compile failed");
    assert!(status.success());

    // Linking from objects only, with -flto, is the path that consumes lto_ldflags.
    let exe = tmp.path().join("prog");
    let output = Command::new(cargo_bin("rllvm-cc"))
        .env("RLLVM_CONFIG", &cfg)
        .env("RLLVM_LTO_MODE", "skip")
        .args(["-flto", "-o"])
        .arg(&exe)
        .arg(&obj)
        .output()
        .expect("link failed");
    assert!(
        output.status.success(),
        "LTO link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(exe.exists());
}

/// `rllvm-rustc` compiles Rust to an object and embeds a bitcode path.
///
/// The pure helpers in the rustc wrapper are unit-tested, but `run()` itself —
/// the pass-through invocation, the `--emit=llvm-bc` re-invocation, and the
/// embedding — had no coverage at all.
#[test]
fn rustc_wrapper_emits_and_embeds_bitcode() {
    let rustc = which("rustc").expect("rustc not found");
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("lib.rs");
    fs::write(&src, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
    let obj = tmp.path().join("lib.o");

    let output = rllvm("rllvm-rustc")
        .arg(&rustc)
        .args(["--crate-type=lib", "--emit=obj", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-rustc");
    assert!(
        output.status.success(),
        "rustc wrapper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(obj.exists(), "no object produced");

    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&obj)
        .expect("failed to read the embedded section");
    assert_eq!(paths.len(), 1, "expected one embedded bitcode path");
    assert!(
        paths[0].exists(),
        "embedded bitcode missing: {:?}",
        paths[0]
    );
    assert_bitcode_magic(&paths[0]);
}

/// Query invocations must skip bitcode generation entirely.
#[test]
fn rustc_wrapper_skips_bitcode_for_query_invocations() {
    let rustc = which("rustc").expect("rustc not found");

    for query in ["--version", "--print=sysroot"] {
        let output = rllvm("rllvm-rustc")
            .arg(&rustc)
            .arg(query)
            .output()
            .expect("Failed to run rllvm-rustc");
        assert!(
            output.status.success(),
            "{query} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty(), "{query} produced no output");
    }
}

/// A failing rustc invocation propagates its exit code without embedding.
#[test]
fn rustc_wrapper_propagates_failure() {
    let rustc = which("rustc").expect("rustc not found");
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("bad.rs");
    fs::write(&src, "this is not valid rust\n").unwrap();

    let output = rllvm("rllvm-rustc")
        .arg(&rustc)
        .args(["--crate-type=lib", "--emit=obj"])
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-rustc");
    assert!(
        !output.status.success(),
        "a broken crate was reported as success"
    );
}

/// `rllvm-init` reports a config directory it cannot create.
#[test]
fn init_reports_an_uncreatable_config_directory() {
    let tmp = TempDir::new().unwrap();
    // A regular file where a directory would have to be.
    let blocker = tmp.path().join("blocker");
    fs::write(&blocker, b"not a directory").unwrap();

    let output = Command::new(cargo_bin("rllvm-init"))
        .arg("-o")
        .arg(blocker.join("config.toml"))
        .env("HOME", tmp.path())
        .output()
        .expect("Failed to run rllvm-init");
    assert!(!output.status.success(), "an uncreatable path was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "panicked: {stderr}");
}

/// `rllvm-init` reports a config path it cannot write.
#[test]
fn init_reports_an_unwritable_config_path() {
    let tmp = TempDir::new().unwrap();
    // Writing to a directory fails.
    let dir_as_target = tmp.path().join("a_directory");
    fs::create_dir_all(&dir_as_target).unwrap();

    let output = Command::new(cargo_bin("rllvm-init"))
        .arg("-o")
        .arg(&dir_as_target)
        .env("HOME", tmp.path())
        .output()
        .expect("Failed to run rllvm-init");
    assert!(
        !output.status.success(),
        "writing to a directory was accepted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "panicked: {stderr}");
}

/// `rllvm-get-bc` reports an input it cannot read.
#[test]
fn get_bc_reports_an_unreadable_input() {
    let tmp = TempDir::new().unwrap();
    // A directory canonicalizes fine but cannot be read as a file.
    let dir = tmp.path().join("a_directory");
    fs::create_dir_all(&dir).unwrap();

    let output = rllvm("rllvm-get-bc")
        .arg(&dir)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        !output.status.success(),
        "a directory was accepted as input"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "panicked: {stderr}");
}

/// True when the local clang can target WebAssembly.
fn wasm_target_available() -> bool {
    let Some(llvm_config) = find_llvm_config() else {
        return false;
    };
    let clang = llvm_config.parent().unwrap().join("clang");
    Command::new(clang)
        .args(["--print-targets"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32"))
        .unwrap_or(false)
}

/// True when a WebAssembly linker is on PATH.
fn wasm_ld_available() -> bool {
    which("wasm-ld").is_ok()
}

/// A WebAssembly object carries the embedded bitcode path.
#[test]
fn wasm_object_carries_the_bitcode_path() {
    if !wasm_target_available() {
        eprintln!("skipping: clang has no wasm32 target");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let obj = tmp.path().join("wasm_lib.o");

    let output = rllvm("rllvm-cc")
        .args(["--target=wasm32-unknown-unknown", "-c", "-o"])
        .arg(&obj)
        .arg(fixture("wasm_lib.c"))
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "wasm compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&obj)
        .expect("failed to read the embedded section");
    assert_eq!(paths.len(), 1, "expected one embedded path");
    assert_bitcode_magic(&paths[0]);
}

/// The section survives `wasm-ld` and concatenates across translation units.
///
/// This is the whole bitcode-path contract on WebAssembly, and it did not work
/// before the section was renamed: `lld/wasm/Writer.cpp` skips `.llvmbc` and
/// `.llvmcmd` by name, because those belong to `clang -fembed-bitcode`, while
/// concatenating every other custom section. rllvm used `.llvmbc`, so the
/// linker dropped it and extraction from a linked module always failed.
#[test]
fn wasm_linked_module_carries_every_translation_unit() {
    if !wasm_target_available() || !wasm_ld_available() {
        eprintln!("skipping: needs a wasm32 target and wasm-ld");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let lib_obj = tmp.path().join("wasm_lib.o");
    let main_obj = tmp.path().join("wasm_main.o");

    for (src, obj) in [("wasm_lib.c", &lib_obj), ("wasm_main.c", &main_obj)] {
        let status = rllvm("rllvm-cc")
            .args(["--target=wasm32-unknown-unknown", "-c", "-o"])
            .arg(obj)
            .arg(fixture(src))
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "wasm compile of {src} failed");
    }

    let module = tmp.path().join("out.wasm");
    let output = rllvm("rllvm-cc")
        .args([
            "--target=wasm32-unknown-unknown",
            "-nostdlib",
            "-Wl,--no-entry",
            "-Wl,--export-all",
            "-o",
        ])
        .arg(&module)
        .arg(&lib_obj)
        .arg(&main_obj)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "wasm link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Both translation units must be listed in the linked module.
    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&module)
        .expect("failed to read the embedded section from the linked module");
    assert_eq!(
        paths.len(),
        2,
        "linker did not concatenate both paths; got {paths:?}"
    );

    // And the merged bitcode must contain both functions.
    let bitcode = tmp.path().join("out.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&module)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "extraction from the linked wasm failed");
    assert_bitcode_magic(&bitcode);
}

/// The embedded section survives a link that dead strips.
///
/// rllvm's section is unreferenced, so `-dead_strip` is entitled to discard it.
/// rllvm used to buy survival by deleting the flag from the user's link, which
/// silently handed them a binary they had not asked for. Marking the section
/// `S_ATTR_NO_DEAD_STRIP` keeps it instead, so the flag can be passed through.
///
/// The stripped-symbol assertion is the load-bearing half. Without it the test
/// passes while the flag is still being dropped, because a link that never
/// dead strips trivially preserves the section. `-dead_strip` is an ld64 flag,
/// so this is Mach-O only; ELF sections added by rllvm are non-allocatable and
/// were never `--gc-sections` candidates.
#[test]
#[cfg(target_os = "macos")]
fn bitcode_survives_a_dead_stripping_link() {
    const UNUSED: &str = "rllvm_unreferenced_probe";

    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("lib.c"),
        format!("int helper(int x) {{ return x + 1; }}\nint {UNUSED}(int x) {{ return x * 7; }}\n"),
    )
    .unwrap();
    fs::write(
        tmp.path().join("main.c"),
        "int helper(int);\nint main(void) { return helper(41) == 42 ? 0 : 1; }\n",
    )
    .unwrap();

    let mut objects = Vec::new();
    for name in ["lib", "main"] {
        let object = tmp.path().join(format!("{name}.o"));
        let status = rllvm("rllvm-cc")
            .args(["-c", "-o"])
            .arg(&object)
            .arg(tmp.path().join(format!("{name}.c")))
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "compiling {name}.c failed");
        objects.push(object);
    }

    let exe = tmp.path().join("prog");
    let output = rllvm("rllvm-cc")
        .arg("-Wl,-dead_strip")
        .arg("-o")
        .arg(&exe)
        .args(&objects)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The flag has to reach the linker, or the rest of this test is vacuous.
    let image = fs::read(&exe).expect("cannot read the linked binary");
    let stripped = !image.windows(UNUSED.len()).any(|w| w == UNUSED.as_bytes());
    assert!(
        stripped,
        "`{UNUSED}` is still in the binary, so -dead_strip never reached the linker"
    );

    // And the section has to outlive the stripping.
    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&exe)
        .expect("no embedded section survived the dead-stripping link");
    assert_eq!(
        paths.len(),
        2,
        "dead stripping removed embedded paths; got {paths:?}"
    );
}

/// A linked binary carries its bitcode path, recorded through a marker object.
///
/// rustc gives no chance to patch the objects it links, so the path rides in
/// on `-C link-arg` instead. Patching the finished binary is not an option: on
/// Darwin it invalidates the code signature and the binary is killed on sight.
#[test]
fn rustc_wrapper_embeds_into_a_linked_binary() {
    let rustc = which("rustc").expect("rustc not found");
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("main.rs");
    fs::write(&src, "fn main() { println!(\"hello\"); }\n").unwrap();
    let exe = tmp.path().join("prog");

    let output = rllvm("rllvm-rustc")
        .arg(&rustc)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-rustc");
    assert!(
        output.status.success(),
        "rustc wrapper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(exe.exists(), "no executable produced");

    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&exe)
        .expect("the linked binary carries no rllvm section");
    assert_eq!(paths.len(), 1, "expected one embedded path, got {paths:?}");
    assert!(
        paths[0].exists(),
        "embedded bitcode missing: {:?}",
        paths[0]
    );
    assert_bitcode_magic(&paths[0]);
}

fn assert_rustc_relative_output(out_dir: bool, relative_record: bool) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    let mut command = rllvm("rllvm-rustc");
    command.current_dir(&root).arg(which("rustc").unwrap());
    let (program, bitcode) = if out_dir {
        fs::create_dir(root.join("out")).unwrap();
        command.args(["--crate-name", "program", "--out-dir", "out"]);
        ("out/program", "out/program.bc")
    } else {
        command.args(["-o", "program"]);
        ("program", "program.bc")
    };
    if relative_record {
        command.env("RLLVM_BITCODE_ROOT", &root);
    } else {
        command.env_remove("RLLVM_BITCODE_ROOT");
    }
    let output = command.arg("main.rs").output().unwrap();
    assert!(
        output.status.success(),
        "relative rustc output failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(root.join(program)).status().unwrap().success());
    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(root.join(program))
        .expect("linked output must record its bitcode");
    let expected = if relative_record {
        PathBuf::from(bitcode)
    } else {
        root.join(bitcode)
    };
    assert_eq!(paths, vec![expected]);
    assert_bitcode_magic(&root.join(bitcode));
}

#[test]
fn rustc_relative_output_is_recorded_before_the_bitcode_exists() {
    assert_rustc_relative_output(false, false);
}

#[test]
fn rustc_relative_out_dir_is_recorded_before_the_bitcode_exists() {
    assert_rustc_relative_output(true, false);
}

#[test]
fn rustc_relative_output_respects_bitcode_root() {
    assert_rustc_relative_output(false, true);
}

/// Recursively copy a directory, so the checked-in fixture never gains a
/// `target/` and parallel test runs cannot race on one.
fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy the cargo fixture somewhere writable and build it under the wrapper.
fn build_cargo_fixture(tmp: &TempDir) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/cargo_fixture");
    let root = tmp.path().join("cargo_fixture");
    copy_dir_all(&fixture, &root).expect("failed to copy the cargo fixture");

    let output = Command::new("cargo")
        .arg("build")
        .current_dir(&root)
        .env("RUSTC_WRAPPER", cargo_bin("rllvm-rustc"))
        .env("RLLVM_CONFIG", shared_config_path())
        // Otherwise the fixture inherits rllvm's own target directory.
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("Failed to run cargo build");
    assert!(
        output.status.success(),
        "cargo build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    root
}

/// #85: under cargo the wrapper produced nothing and reported success.
///
/// Asserting on both crates is the point. A binary carrying only its own path
/// passes a "some bitcode exists" check while dependency bitcode is silently
/// missing, and the two arrive by different routes — the binary's own path
/// through a marker object, the dependency's through its patched rlib.
#[test]
fn cargo_build_embeds_bitcode_for_bin_and_lib() {
    let tmp = TempDir::new().unwrap();
    let root = build_cargo_fixture(&tmp);
    let exe = root.join("target/debug/myapp");
    assert!(exe.exists(), "cargo did not produce the binary");

    let paths = rllvm::utils::extract_bitcode_filepaths_from_object_file(&exe)
        .expect("the cargo-built binary carries no rllvm section");
    let names: Vec<String> = paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();

    assert!(
        names.iter().any(|name| name.starts_with("myapp")),
        "the binary's own bitcode is missing: {names:?}"
    );
    assert!(
        names.iter().any(|name| name.starts_with("mylib")),
        "the dependency's bitcode is missing: {names:?}"
    );

    let bitcode = tmp.path().join("whole.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(&exe)
        .arg("-o")
        .arg(&bitcode)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_bitcode_magic(&bitcode);
}

/// A library crate is usable on its own, without a binary to link it into.
#[test]
fn cargo_build_bitcode_extractable_from_rlib() {
    let tmp = TempDir::new().unwrap();
    let root = build_cargo_fixture(&tmp);

    let rlib = fs::read_dir(root.join("target/debug/deps"))
        .expect("no deps directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().is_some_and(|ext| ext == "rlib")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("libmylib-"))
        })
        .expect("mylib rlib not found");

    let bitcode = tmp.path().join("mylib.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(&rlib)
        .arg("-o")
        .arg(&bitcode)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "extraction from the rlib failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_bitcode_magic(&bitcode);
}

/// `rustc_filepath` in the config selects the rustc to delegate to.
///
/// Every other tool rllvm drives is a config key. The rustc wrapper used to
/// read `$RLLVM_REAL_RUSTC` and nothing else, so a `rustc_filepath` entry in a
/// shared config had no effect.
///
/// The stand-in has to be distinguishable from the real rustc: asserting that
/// "some rustc ran" passes on the `PATH` fallback whether or not the config is
/// consulted.
#[test]
#[cfg(unix)]
fn rustc_wrapper_honours_rustc_filepath_from_config() {
    use std::os::unix::fs::PermissionsExt;

    let rustc = which("rustc").expect("rustc not found");
    let tmp = TempDir::new().unwrap();

    let stand_in = tmp.path().join("marked-rustc");
    fs::write(
        &stand_in,
        format!(
            "#!/bin/sh\necho rllvm-stand-in-rustc\nexec {} \"$@\"\n",
            rustc.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).unwrap();

    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cfg = tmp.path().join("config.toml");
    fs::write(
        &cfg,
        format!("{base}rustc_filepath = '{}'\n", stand_in.display()),
    )
    .unwrap();

    let output = Command::new(cargo_bin("rllvm-rustc"))
        .env("RLLVM_CONFIG", &cfg)
        .env_remove("RLLVM_REAL_RUSTC")
        .arg("--version")
        .output()
        .expect("Failed to run rllvm-rustc");

    assert!(
        output.status.success(),
        "the configured rustc was not used: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rllvm-stand-in-rustc"),
        "the config was ignored and rustc came from PATH instead: {stdout}"
    );
}

/// `log_level` in the config reaches the rustc wrapper.
///
/// `rllvm-cc` has read this key all along; `rllvm-rustc` looked only at
/// `$RLLVM_LOG_LEVEL`, so raising the level in a config did nothing for Rust
/// builds.
#[test]
fn rustc_wrapper_honours_log_level_from_config() {
    let rustc = which("rustc").expect("rustc not found");
    let tmp = TempDir::new().unwrap();
    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cfg = tmp.path().join("config.toml");
    fs::write(&cfg, format!("{base}log_level = 3\n")).unwrap();

    let src = tmp.path().join("main.rs");
    fs::write(&src, "fn main() {}\n").unwrap();
    let exe = tmp.path().join("prog");

    let output = Command::new(cargo_bin("rllvm-rustc"))
        .env("RLLVM_CONFIG", &cfg)
        .env_remove("RLLVM_LOG_LEVEL")
        .arg(&rustc)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-rustc");
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bitcode="),
        "debug logging from the config did not reach the wrapper: {stderr}"
    );
}

/// `-flto` reports that bitcode generation is skipped, at the default log level.
///
/// The skip itself is legitimate: under LTO the object file already is bitcode.
/// But the linked binary then carries no embedded paths, so `rllvm-get-bc`
/// finds nothing. Reported only through `tracing`, that went unseen at the
/// default level and the build looked like it had worked.
#[test]
fn lto_reports_that_bitcode_generation_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.c");
    fs::write(&src, "int helper(int x) { return x + 1; }\n").unwrap();
    let object = tmp.path().join("a.o");

    // This test predates `lto_mode` and covers the skip path specifically --
    // #97's loud warning is only emitted in `skip` mode now. The default
    // `marker` mode does not skip at all (Task 5's tests cover that path).
    let output = rllvm("rllvm-cc")
        .env("RLLVM_LTO_MODE", "skip")
        .args(["-flto", "-c", "-o"])
        .arg(&object)
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("link-time optimization"),
        "the skip was not reported at the default log level: {stderr:?}"
    );
}

/// The routine skips stay quiet.
///
/// Preprocessing, dependency generation and link-only invocations all skip
/// bitcode generation and all happen constantly. Reporting them would put a
/// warning in front of every build, which is how warnings stop being read.
#[test]
fn routine_skips_are_not_reported() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.c");
    fs::write(&src, "int helper(int x) { return x + 1; }\n").unwrap();

    let output = rllvm("rllvm-cc")
        .arg("-E")
        .arg(&src)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(output.status.success(), "preprocessing failed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("warning:"),
        "preprocessing should not warn about skipped bitcode: {stderr:?}"
    );
}

/// Writes three C files that must all reach the extracted module: two leaf
/// functions and a `main` that calls both.
fn write_lto_sources(dir: &Path) -> Vec<PathBuf> {
    let a = dir.join("lto_a.c");
    let b = dir.join("lto_b.c");
    let main = dir.join("lto_main.c");
    fs::write(&a, "int a_fn(int x) { return x + 1; }\n").unwrap();
    fs::write(&b, "int b_fn(int x) { return x * 2; }\n").unwrap();
    fs::write(
        &main,
        "int a_fn(int); int b_fn(int);\nint main(void) { return a_fn(b_fn(3)) - 7; }\n",
    )
    .unwrap();
    vec![a, b, main]
}

/// Compiles each source to an object with `rllvm-cc`, links them, extracts the
/// bitcode, and returns the disassembled module's symbol listing.
fn lto_build_and_extract(tmp: &Path, lto_flag: &str, extra_link: &[&str]) -> String {
    let sources = write_lto_sources(tmp);
    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .args(["--", lto_flag, "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.join("prog");
    let mut link = rllvm("rllvm-cc");
    link.arg("--")
        .arg(lto_flag)
        .args(extra_link)
        .arg("-o")
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    let status = link.status().expect("Failed to run rllvm-cc");
    assert!(status.success(), "LTO link failed");

    let bitcode = tmp.join("prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on the LTO binary");
    assert_bitcode_magic(&bitcode);

    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg(&bitcode)
        .output()
        .expect("llvm-nm failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn lto_full_binary_yields_whole_program_bitcode() {
    let tmp = TempDir::new().unwrap();
    let symbols = lto_build_and_extract(tmp.path(), "-flto", &[]);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from:\n{symbols}"
        );
    }
}

#[test]
fn lto_thin_binary_yields_whole_program_bitcode() {
    let tmp = TempDir::new().unwrap();
    let symbols = lto_build_and_extract(tmp.path(), "-flto=thin", &[]);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from:\n{symbols}"
        );
    }
}

#[test]
#[cfg(target_vendor = "apple")]
fn lto_marker_survives_dead_strip() {
    // Nothing references the section, so a dead-stripping link discards it
    // unless the directive says `no_dead_strip`.
    let tmp = TempDir::new().unwrap();
    let symbols = lto_build_and_extract(tmp.path(), "-flto", &["-Wl,-dead_strip"]);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from:\n{symbols}"
        );
    }
}

#[test]
fn lto_and_non_lto_objects_share_one_section() {
    // `ld.bfd` emits two same-named sections when their flags disagree, and
    // reading only the first loses the non-LTO half of the build.
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let plain = tmp.path().join("lto_c.c");
    fs::write(&plain, "int c_fn(int x) { return x - 1; }\n").unwrap();
    let main = tmp.path().join("lto_main.c");
    fs::write(
        &main,
        "int a_fn(int); int b_fn(int); int c_fn(int);\n\
         int main(void) { return a_fn(b_fn(3)) - c_fn(8); }\n",
    )
    .unwrap();

    let mut objects = vec![];
    for (source, flags) in [
        (&sources[0], vec!["-flto"]),
        (&sources[1], vec!["-flto"]),
        (&main, vec!["-flto"]),
        (&plain, vec![]),
    ] {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .arg("--")
            .args(&flags)
            .args(["-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("mixed");
    let mut link = rllvm("rllvm-cc");
    link.args(["--", "-flto", "-o"]).arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(link.status().unwrap().success(), "mixed link failed");

    let manifest_dir = tmp.path().join("mixed.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-m")
        .arg("-o")
        .arg(&manifest_dir)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on the mixed binary");

    let manifest = fs::read_to_string(tmp.path().join("mixed.bc.manifest")).unwrap();
    assert_eq!(
        manifest.lines().count(),
        4,
        "every translation unit must be recorded:\n{manifest}"
    );
}

/// `-std=c++17` reaches the marker compile through `compile_args`. A marker
/// compiled with the C driver (`clang`, not `clang++`) rejects that flag
/// outright with "invalid argument '-std=c++17' not allowed with 'C'", so
/// this is a hard build failure unless the marker is compiled with the
/// wrapper's own compiler. Every other LTO test in this file uses `rllvm-cc`
/// on `.c` sources and cannot catch this class of bug.
#[test]
fn lto_cxx_build_with_std_flag_succeeds() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("lto_cxx.cpp");
    fs::write(
        &src,
        "int add(int a, int b) { return a + b; }\n\
         int main() { return add(1, 2) - 3; }\n",
    )
    .unwrap();

    let object = src.with_extension("o");
    let status = rllvm("rllvm-cxx")
        .args(["--", "-flto", "-std=c++17", "-c", "-o"])
        .arg(&object)
        .arg(&src)
        .status()
        .expect("Failed to run rllvm-cxx");
    assert!(
        status.success(),
        "rllvm-cxx failed to compile with -flto -std=c++17"
    );

    let program = tmp.path().join("cxx_prog");
    let status = rllvm("rllvm-cxx")
        .args(["--", "-flto", "-std=c++17", "-o"])
        .arg(&program)
        .arg(&object)
        .status()
        .expect("Failed to run rllvm-cxx");
    assert!(status.success(), "LTO link failed");

    let bitcode = tmp.path().join("cxx_prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed on the LTO binary");
    assert_bitcode_magic(&bitcode);

    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg(&bitcode)
        .output()
        .expect("llvm-nm failed");
    let symbols = String::from_utf8_lossy(&output.stdout);
    assert!(symbols.contains("main"), "main missing from:\n{symbols}");
    assert!(
        symbols.contains("add"),
        "the mangled `add` symbol is missing from:\n{symbols}"
    );
}

/// `-ffat-lto-objects` produces a real ELF object with the bitcode inside, so
/// the wrapper's content-based dispatch sends it down the ordinary
/// `llvm-objcopy` path rather than the marker path. Only `is_bitcode_file`'s
/// own unit test covered that decision; nothing asserted that such a build
/// stays extractable end to end.
///
/// Linux only: clang rejects `-ffat-lto-objects` on Darwin.
#[test]
#[cfg(target_os = "linux")]
fn fat_lto_objects_stay_extractable_through_the_object_path() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .args(["--", "-flto", "-ffat-lto-objects", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");

        // The premise of the test, not incidental detail: a fat object is an
        // ELF object, which is what routes it to the objcopy path. A plain
        // `-flto` object starts `BC\xc0\xde` and would take the marker path,
        // making this a duplicate of the tests above without anyone noticing.
        let bytes = fs::read(&object).unwrap();
        assert_eq!(
            &bytes[..4],
            b"\x7fELF",
            "expected a fat LTO object at {object:?}"
        );

        objects.push(object);
    }

    // Linked without `-flto`, so the linker takes the machine-code half and
    // the embedded section rides through like any other object's. Linking the
    // same objects *with* `-flto` loses it -- see the ignored test below.
    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.args(["--", "-o"]).arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(link.status().unwrap().success(), "fat LTO link failed");

    let bitcode = tmp.path().join("prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        status.success(),
        "rllvm-get-bc failed on the fat LTO binary"
    );
    assert_bitcode_magic(&bitcode);

    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg("--defined-only")
        .arg(&bitcode)
        .output()
        .expect("llvm-nm failed");
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from:\n{symbols}"
        );
    }
}

/// The other half of `-ffat-lto-objects`: linking fat objects **with** `-flto`.
///
/// Which half of a fat object the linker consumes is decided at link time.
/// GNU ld's plugin generates code from the bitcode and discards the rest of
/// the object, taking the embedded section with it; lld defaults to
/// `--no-fat-lto-objects` and links the machine code, keeping it. Both are
/// exercised, because recording only one half passes under one linker and
/// fails under the other -- which is how #115 shipped.
#[test]
#[cfg(target_os = "linux")]
fn fat_lto_objects_linked_with_lto_stay_extractable() {
    for linker in ["-fuse-ld=bfd", "-fuse-ld=lld"] {
        fat_lto_link_records_every_unit(linker);
    }
}

#[cfg(target_os = "linux")]
fn fat_lto_link_records_every_unit(linker: &str) {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .args(["--", "-flto", "-ffat-lto-objects", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.args(["--", "-flto", linker, "-o"]).arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(
        link.status().unwrap().success(),
        "fat LTO link failed under {linker}"
    );

    let bitcode = tmp.path().join("prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        status.success(),
        "rllvm-get-bc found no bitcode in the fat LTO binary linked with {linker}"
    );
    assert_bitcode_magic(&bitcode);

    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg("--defined-only")
        .arg(&bitcode)
        .output()
        .expect("llvm-nm failed");
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing under {linker}:\n{symbols}"
        );
    }
}

/// A universal build fails in two different places with two different
/// unhelpful messages -- `ExecutionFailure` from the bitcode compile, or
/// `ObjectReadError("Unsupported file format")` from embedding into a marker
/// object that came out universal. Neither says "architecture".
///
/// Darwin only: `-arch` is a Mach-O driver option.
#[test]
#[cfg(target_vendor = "apple")]
fn universal_build_names_the_architectures_it_cannot_handle() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("u.c");
    fs::write(&source, "int u_fn(int x) { return x + 1; }\n").unwrap();
    let object = tmp.path().join("u.o");

    // No `-flto` anywhere: the limitation is the bitcode compile, not LTO.
    let output = rllvm("rllvm-cc")
        .args(["--", "-arch", "x86_64", "-arch", "arm64", "-c", "-o"])
        .arg(&object)
        .arg(&source)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        !output.status.success(),
        "a universal build must not claim success"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["universal build", "x86_64", "arm64"] {
        assert!(
            stderr.contains(expected),
            "the error must name {expected}:\n{stderr}"
        );
    }
    // `marker` is not an escape from this -- the bitcode compile fails there
    // too -- so the message must not send anyone there.
    assert!(
        !stderr.contains("lto_mode = \"marker\""),
        "must not recommend a mode that fails the same way:\n{stderr}"
    );
}

/// The same diagnostic from the other direction: under `save-temps` the link
/// builds a marker object, and with two `-arch` values it comes out universal.
#[test]
#[cfg(target_vendor = "apple")]
fn universal_save_temps_link_names_the_architectures() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("u.c");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();
    let object = tmp.path().join("u.o");
    assert!(
        rllvm("rllvm-cc")
            .args(["--", "-arch", "arm64", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );

    let output = rllvm("rllvm-cc")
        .env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-arch", "x86_64", "-arch", "arm64", "-flto", "-o"])
        .arg(tmp.path().join("prog"))
        .arg(&object)
        .output()
        .expect("Failed to run rllvm-cc");
    assert!(
        !output.status.success(),
        "a universal link must not claim success"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["universal build", "x86_64", "arm64"] {
        assert!(
            stderr.contains(expected),
            "the error must name {expected}:\n{stderr}"
        );
    }
}

/// Compiling under `marker` and linking under `save-temps` leaves the binary
/// naming both the per-unit modules and the collected one, so extraction
/// merges those translation units twice. The symptom appears in `rllvm-get-bc`
/// -- a different tool from the one given the wrong mode -- so the link warns.
#[test]
fn save_temps_link_warns_about_objects_built_under_marker() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    // Compiled under the default `marker` mode.
    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-flto", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    let output = link.output().expect("Failed to run rllvm-cc");
    assert!(output.status.success(), "the link must still succeed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("per-unit bitcode path"),
        "expected a warning naming the mode mismatch:\n{stderr}"
    );
    assert!(
        stderr.contains("merge those translation units twice"),
        "the warning must say what goes wrong:\n{stderr}"
    );
}

/// Completions are generated from the same definitions the binaries parse
/// with, so this asserts the property that broke: the script offers the flags
/// that exist, and none of the ones removed in v0.1.7.
#[test]
fn completions_describe_the_wrapper_that_actually_ships() {
    let output = rllvm("rllvm-completions")
        .args(["--shell", "zsh", "--bin", "cc"])
        .output()
        .expect("Failed to run rllvm-completions");
    assert!(output.status.success());
    let script = String::from_utf8_lossy(&output.stdout);

    for expected in [
        "--rllvm-compiler",
        "--rllvm-verbose",
        "--rllvm-help",
        "--rllvm-version",
    ] {
        assert!(
            script.contains(expected),
            "{expected} missing from:\n{script}"
        );
    }

    // `-c` is compile-only and `-v` is the compiler's verbose flag. Offering
    // either -- as the hand-written generator did -- completes a path where the
    // build system expects an object.
    for removed in ["'-c+[", "'-v[", "'--compiler=[", "'--verbose["] {
        assert!(
            !script.contains(removed),
            "{removed} belongs to the compiler:\n{script}"
        );
    }
}

/// `rllvm-rustc` answers the two informational flags a person runs by hand.
/// Cargo owns the rest of its command line, so it has no other options -- but
/// it used to have none at all, and `--rllvm-version` fell through to the
/// bitcode-path logic and complained about `--out-dir`.
#[test]
fn rustc_wrapper_answers_its_own_informational_flags() {
    for flag in ["--rllvm-version", "--rllvm-help"] {
        let output = rllvm("rllvm-rustc")
            .arg(flag)
            .output()
            .expect("Failed to run rllvm-rustc");
        assert!(output.status.success(), "{flag} failed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("rllvm-rustc"),
            "{flag} must answer for the wrapper:\n{stdout}"
        );
        assert!(
            !stdout.contains("out-dir"),
            "{flag} reached the bitcode-path logic:\n{stdout}"
        );
    }
}

/// `--rllvm-verbose` is shared with `rllvm-cc`, so it has to work here too --
/// and be removed before rustc, which would reject it.
#[test]
fn rustc_wrapper_honours_verbose_without_passing_it_on() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("lib.rs");
    fs::write(&source, "pub fn f() -> i32 { 1 }\n").unwrap();

    let output = rllvm("rllvm-rustc")
        .args(["--rllvm-verbose=3", "rustc", "--crate-type", "lib"])
        .args(["--crate-name", "verbose_probe", "--out-dir"])
        .arg(tmp.path())
        .arg(&source)
        .output()
        .expect("Failed to run rllvm-rustc");

    assert!(
        output.status.success(),
        "the flag must not reach rustc:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        tmp.path().join("libverbose_probe.rlib").exists(),
        "rustc still has to build the crate"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rllvm-rustc:"),
        "level 3 must log the wrapper's own debug line:\n{stderr}"
    );
}

/// `rllvm-get-bc` grew `--merge-strategy` and `--bitcode-root`; the
/// hand-written generator never learned about either.
#[test]
fn completions_cover_every_extraction_option() {
    let output = rllvm("rllvm-completions")
        .args(["--shell", "zsh", "--bin", "get-bc"])
        .output()
        .expect("Failed to run rllvm-completions");
    assert!(output.status.success());
    let script = String::from_utf8_lossy(&output.stdout);

    for expected in [
        "--merge-strategy",
        "--bitcode-root",
        "--output",
        "--save-manifest",
    ] {
        assert!(
            script.contains(expected),
            "{expected} missing from:\n{script}"
        );
    }
}

/// Every binary with a CLI of its own, for every shell, produces a script that
/// names that binary. `rllvm-cxx` is the one that can go wrong quietly: it
/// shares its argument struct with `rllvm-cc`, so an unnamed command would
/// define `_rllvm-cc` in the file installed for `rllvm-cxx`.
#[test]
fn completions_are_generated_for_every_binary_and_shell() {
    for bin in [
        "cc",
        "cxx",
        "get-bc",
        "init",
        "info",
        "rustc",
        "completions",
    ] {
        for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
            let output = rllvm("rllvm-completions")
                .args(["--shell", shell, "--bin", bin])
                .output()
                .expect("Failed to run rllvm-completions");
            assert!(output.status.success(), "{bin}/{shell} failed");

            let script = String::from_utf8_lossy(&output.stdout);
            assert!(!script.trim().is_empty(), "{bin}/{shell} produced nothing");
            assert!(
                script.contains(&format!(
                    "rllvm-{}",
                    if bin == "get-bc" { "get-bc" } else { bin }
                )),
                "{bin}/{shell} does not name the binary:\n{script}"
            );
        }
    }
}

/// `lto_mode = "save-temps"` collects the module the linker's own LTO
/// pipeline merged, instead of recording per-unit paths with a marker.
#[test]
fn save_temps_mode_records_the_linker_merged_module() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-flto", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(link.status().unwrap().success(), "save-temps link failed");

    let merged = tmp.path().join("prog.rllvm.bc");
    assert!(merged.exists(), "the merged module was not collected");
    assert_bitcode_magic(&merged);

    // The collected module must actually be whole-program, not just some
    // bitcode file that happened to be lying around.
    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg(&merged)
        .output()
        .expect("llvm-nm failed");
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from the collected module:\n{symbols}"
        );
    }

    // The binary names it, so extraction needs no discovery convention.
    let extracted = tmp.path().join("prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-m")
        .arg("-o")
        .arg(&extracted)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc failed under save-temps");
    let manifest = fs::read_to_string(tmp.path().join("prog.bc.manifest")).unwrap();
    assert!(
        manifest.trim().ends_with("prog.rllvm.bc"),
        "the binary must name the merged module:\n{manifest}"
    );
}

/// A combined source build must record the module produced by its LTO link.
fn save_temps_source_link_and_extract(
    output_name: Option<&str>,
    with_object: bool,
    language: Option<&str>,
) {
    let tmp = TempDir::new().unwrap();
    let mut inputs = write_lto_sources(tmp.path());
    if let Some(language) = language {
        for input in &mut inputs {
            if language == "c++" {
                let source = fs::read_to_string(&*input)
                    .unwrap()
                    .replace("int a_fn", "extern \"C\" int a_fn")
                    .replace("int b_fn", "extern \"C\" int b_fn");
                fs::write(&*input, source).unwrap();
            } else {
                // The explicit language must still apply to the user's inputs.
                let renamed = input.with_extension("cpp");
                fs::rename(&*input, &renamed).unwrap();
                *input = renamed;
            }
        }
    }
    if with_object {
        let object = inputs[0].with_extension("o");
        let output = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["-flto", "-c"])
            .arg(&inputs[0])
            .arg("-o")
            .arg(&object)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        inputs[0] = object;
    }

    let mut build = rllvm(if language == Some("c++") {
        "rllvm-cxx"
    } else {
        "rllvm-cc"
    });
    build
        .current_dir(tmp.path())
        .env("RLLVM_LTO_MODE", "save-temps")
        .arg("-flto");
    if let Some(language) = language {
        build.args(["-x", language]);
    }
    build.args(&inputs);
    if let Some(name) = output_name {
        build.args(["-o", name]);
    }
    let output = build.output().unwrap();
    assert!(
        output.status.success(),
        "source/link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let name = output_name.unwrap_or("a.out");
    let program = tmp.path().join(name);
    assert!(Command::new(&program).status().unwrap().success());

    let extracted = tmp.path().join("extracted.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(&program)
        .args(["-m", "-o"])
        .arg(&extracted)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "source/link extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_bitcode_magic(&extracted);
    let manifest = fs::read_to_string(tmp.path().join("extracted.bc.manifest")).unwrap();
    let paths: Vec<_> = manifest.lines().collect();
    assert_eq!(
        paths.len(),
        1,
        "expected only the merged module: {manifest}"
    );
    assert_eq!(
        Path::new(paths[0]).canonicalize().unwrap(),
        tmp.path()
            .join(format!("{name}.rllvm.bc"))
            .canonicalize()
            .unwrap()
    );
    let output = Command::new(find_llvm_nm().expect("llvm-nm not found"))
        .arg("--defined-only")
        .arg(&extracted)
        .output()
        .unwrap();
    assert!(output.status.success());
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.lines().any(|line| line
                .split_whitespace()
                .last()
                .map(|name| name.trim_start_matches('_'))
                == Some(symbol)),
            "missing definition of {symbol}: {symbols}"
        );
    }
}

#[test]
fn save_temps_source_link_records_module_for_relative_output() {
    save_temps_source_link_and_extract(Some("prog"), false, None);
}

#[test]
fn save_temps_source_link_records_module_for_default_output() {
    save_temps_source_link_and_extract(None, false, None);
}

#[test]
fn save_temps_source_link_includes_lto_object_definitions() {
    save_temps_source_link_and_extract(Some("prog"), true, None);
}

#[test]
fn save_temps_source_link_preserves_explicit_c_language() {
    save_temps_source_link_and_extract(Some("prog"), false, Some("c"));
}

#[test]
fn save_temps_source_link_preserves_explicit_cxx_language() {
    save_temps_source_link_and_extract(Some("prog"), false, Some("c++"));
}

#[test]
fn save_temps_source_link_warns_that_thin_lto_has_no_merged_module() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());
    let output = rllvm("rllvm-cc")
        .current_dir(tmp.path())
        .env("RLLVM_LTO_MODE", "save-temps")
        .arg("-flto=thin")
        .args(&sources)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ThinLTO source/link failed: {stderr}"
    );
    assert!(
        stderr.contains("ThinLTO builds no whole-program module"),
        "missing ThinLTO diagnostic: {stderr}"
    );
    assert!(
        Command::new(tmp.path().join("a.out"))
            .status()
            .unwrap()
            .success()
    );
    assert!(!tmp.path().join("a.out.rllvm.bc").exists());
}

/// Modes that stop before linking must not receive a marker or linker flags.
#[test]
fn save_temps_does_not_intercept_non_linking_source_invocations() {
    let mut failures = Vec::new();
    for flag in [
        "-c",
        "-E",
        "-S",
        "-M",
        "-MM",
        "--version",
        "--help",
        "-###",
        "-fsyntax-only",
        "-print-prog-name=ld",
        "--help-hidden",
        "-dumpmachine",
        "--analyze",
        "-dumpversion",
        "-emit-ast",
        "-ccc-print-phases",
        "-fdriver-only",
    ] {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main.c");
        fs::write(&source, "int main(void) { return 0; }\n").unwrap();
        let output = rllvm("rllvm-cc")
            .current_dir(tmp.path())
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--rllvm-verbose=3", "-flto", flag])
            .arg(&source)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() || stderr.contains("rllvm_marker") {
            failures.push(format!("{flag} failed or received a link marker: {stderr}"));
            continue;
        }
        assert!(
            !tmp.path().join("a.out").exists(),
            "{flag} unexpectedly linked"
        );
        assert!(
            !tmp.path().join("a.out.rllvm.bc").exists(),
            "{flag} collected a module"
        );
        if flag == "-c" {
            assert_bitcode_magic(&tmp.path().join("main.o"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn save_temps_does_not_intercept_configure_only_source_link() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "{}\nis_configure_only = true\n",
            fs::read_to_string(shared_config_path()).unwrap()
        ),
    )
    .unwrap();
    let output = rllvm("rllvm-cc")
        .current_dir(tmp.path())
        .env("RLLVM_CONFIG", config)
        .env("RLLVM_LTO_MODE", "save-temps")
        .args(["--rllvm-verbose=3", "-flto"])
        .args(&sources)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "configure build failed: {stderr}");
    assert!(
        !stderr.contains("rllvm_marker"),
        "configure build received a marker: {stderr}"
    );
    assert!(
        Command::new(tmp.path().join("a.out"))
            .status()
            .unwrap()
            .success()
    );
    assert!(!tmp.path().join("a.out.rllvm.bc").exists());
}

/// `collect_saved_module` has an arm that `llvm-link`s several saved modules
/// together, reached only when the link used more than one LTO partition. A
/// default link saves exactly one module and takes the single-match arm, so
/// the merge arm is dark even on Linux CI.
///
/// Linux only: `--lto-partitions` is an lld option, hence `-fuse-ld=lld`.
#[test]
#[cfg(target_os = "linux")]
fn save_temps_merges_every_lto_partition() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args([
            "--",
            "-flto",
            "-fuse-ld=lld",
            "-Wl,--lto-partitions=2",
            "-o",
        ])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(
        link.status().unwrap().success(),
        "two-partition save-temps link failed"
    );

    let merged = tmp.path().join("prog.rllvm.bc");
    assert!(merged.exists(), "the merged module was not collected");
    assert_bitcode_magic(&merged);

    // Both partitions have to be in it. Keeping only one loses whichever
    // translation units codegen assigned to the other.
    // `--defined-only` is what makes this assertion load-bearing. lld puts
    // `a_fn` in one partition and `b_fn`/`main` in the other, and a merge that
    // kept only the first would still list the dropped symbol as an undefined
    // reference -- so a bare `contains` passes with the merge arm removed.
    let nm = find_llvm_nm().expect("llvm-nm not found");
    let output = Command::new(nm)
        .arg("--defined-only")
        .arg(&merged)
        .output()
        .expect("llvm-nm failed");
    let symbols = String::from_utf8_lossy(&output.stdout);
    for symbol in ["a_fn", "b_fn", "main"] {
        assert!(
            symbols.contains(symbol),
            "{symbol} missing from the merged partitions:\n{symbols}"
        );
    }

    // The per-partition modules are consumed by the merge, not left behind.
    let leftovers: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with("precodegen.bc"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "save-temps partitions were left behind: {leftovers:?}"
    );
}

/// A relative `-o` (`cc -o prog *.o`, run from inside the build directory --
/// the common case, and the one the test above avoids by always using an
/// absolute `tmp.path().join(...)` output) must not crash before the link
/// even runs.
///
/// `<output>.rllvm.bc` does not exist until `collect_saved_module` creates it
/// after the link, so the marker's embedded path has to become absolute
/// without ever requiring the file to already exist; and afterwards,
/// `Path::parent` returns `Some("")`, not `None`, for a bare filename, so the
/// collection step's directory lookup has to guard against that too. Revert
/// either fix and this is the test that fails: `output.is_absolute()` is
/// always true in the sibling tests above, so the fixed code path never runs
/// there.
#[test]
fn save_temps_mode_collects_the_module_for_a_relative_output() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object_name = format!("{}.o", source.file_stem().unwrap().to_string_lossy());
        let status = rllvm("rllvm-cc")
            .current_dir(tmp.path())
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object_name)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object_name);
    }

    let mut link = rllvm("rllvm-cc");
    link.current_dir(tmp.path())
        .env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-flto", "-o", "prog"]);
    for object in &objects {
        link.arg(object);
    }
    assert!(
        link.status().unwrap().success(),
        "save-temps link with a relative -o failed"
    );

    let merged = tmp.path().join("prog.rllvm.bc");
    assert!(
        merged.exists(),
        "the merged module was not collected for a relative -o"
    );
    assert_bitcode_magic(&merged);
}

/// ThinLTO never builds a whole-program module, so `save-temps` has nothing
/// to collect. The build must still succeed rather than fail over a mode the
/// user set globally.
/// This exercises the separate-object link; the source/link regression above
/// covers the same diagnostic when compilation and linking share a command.
#[test]
fn save_temps_mode_warns_that_thin_lto_has_no_merged_module() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-flto=thin", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("thin");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-flto=thin", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    let output = link.output().expect("Failed to run rllvm-cc");
    assert!(output.status.success(), "the link must still succeed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ThinLTO builds no whole-program module"),
        "expected a warning naming the limitation:\n{stderr}"
    );
}

/// A bitcode path containing a backslash and a double quote reaches the
/// section byte for byte.
///
/// The marker interpolates the path into a C string literal inside
/// `__asm__(...)`, so the bytes pass two decoders before they land in the
/// section: the C compiler's, then the assembler's. Escaped for one layer
/// only, a backslash is a hard build failure ("invalid escape sequence") and
/// a quote closes the `.ascii` operand early -- and a path holding `\n` would
/// put a real newline into the section, splitting one entry into two garbage
/// paths.
#[test]
fn lto_marker_records_a_path_with_a_backslash_and_a_quote() {
    let tmp = TempDir::new().unwrap();
    let awkward = tmp.path().join(r#"od\d q"d"#);
    fs::create_dir_all(&awkward).unwrap();

    let a = awkward.join("a.c");
    let main = awkward.join("m.c");
    fs::write(&a, "int a_fn(int x) { return x + 1; }\n").unwrap();
    fs::write(
        &main,
        "int a_fn(int);\nint main(void) { return a_fn(1) - 2; }\n",
    )
    .unwrap();

    let mut objects = vec![];
    for source in [&a, &main] {
        let object = source.with_extension("o");
        let output = rllvm("rllvm-cc")
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .output()
            .expect("Failed to run rllvm-cc");
        assert!(
            output.status.success(),
            "compiling {source:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.args(["--", "-flto", "-o"]).arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(link.status().unwrap().success(), "LTO link failed");

    let extracted = tmp.path().join("prog.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-m")
        .arg("-o")
        .arg(&extracted)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // What the compile actually wrote, against what the binary recorded.
    let mut expected: Vec<PathBuf> = fs::read_dir(&awkward)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bc"))
        .collect();
    expected.sort();
    assert_eq!(expected.len(), 2, "expected one bitcode file per source");

    let manifest = fs::read_to_string(tmp.path().join("prog.bc.manifest")).unwrap();
    let mut recorded: Vec<PathBuf> = manifest.lines().map(PathBuf::from).collect();
    recorded.sort();
    assert_eq!(
        recorded, expected,
        "the recorded paths must match the bitcode files byte for byte"
    );
}

/// An LTO object records a path relative to `bitcode_root`, like every other
/// object rllvm writes.
///
/// The marker path bypassed the resolution every other writer goes through,
/// so one binary could carry an absolute entry for its `-flto` units and a
/// relative one for the rest -- working on the build machine and breaking in
/// a relocated tree, which is what `relocated_build_tree_fails_without_bitcode_root`
/// exists to prevent.
#[test]
fn lto_marker_records_a_path_relative_to_bitcode_root() {
    let tmp = TempDir::new().unwrap();
    let build_a = tmp.path().join("a");
    fs::create_dir_all(&build_a).unwrap();

    let a = build_a.join("lto_a.c");
    let main = build_a.join("lto_main.c");
    fs::write(&a, "int a_fn(int x) { return x + 1; }\n").unwrap();
    fs::write(
        &main,
        "int a_fn(int);\nint main(void) { return a_fn(1) - 2; }\n",
    )
    .unwrap();

    let mut objects = vec![];
    for source in [&a, &main] {
        let object = source.with_extension("o");
        let output = rllvm("rllvm-cc")
            .env("RLLVM_BITCODE_ROOT", &build_a)
            .args(["--", "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .output()
            .expect("Failed to run rllvm-cc");
        assert!(
            output.status.success(),
            "compiling {source:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        objects.push(object);
    }

    let program = build_a.join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_BITCODE_ROOT", &build_a)
        .args(["--", "-flto", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    assert!(link.status().unwrap().success(), "LTO link failed");

    // The build directory moves; the binary and its bitcode travel together.
    let build_b = tmp.path().join("b");
    fs::rename(&build_a, &build_b).unwrap();

    let extracted = tmp.path().join("prog.bc");
    let output = rllvm("rllvm-get-bc")
        .arg(build_b.join("prog"))
        .arg("--bitcode-root")
        .arg(&build_b)
        .arg("-o")
        .arg(&extracted)
        .output()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        output.status.success(),
        "an LTO build recorded an absolute path despite bitcode_root: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_bitcode_magic(&extracted);
}

/// Whether this machine can compile and link for `arch`.
///
/// The macOS SDK carries both architectures, so the answer is normally yes;
/// probing rather than assuming keeps a stripped-down toolchain from failing
/// the suite for a reason that has nothing to do with rllvm.
#[cfg(target_vendor = "apple")]
fn architecture_is_linkable(arch: &str) -> bool {
    let Some(llvm_config) = find_llvm_config() else {
        return false;
    };
    let Ok(output) = Command::new(&llvm_config).arg("--bindir").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let bindir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("probe.c");
    fs::write(&src, "int main(void) { return 0; }\n").unwrap();
    Command::new(bindir.join("clang"))
        .args(["-arch", arch, "-o"])
        .arg(tmp.path().join("probe"))
        .arg(&src)
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
#[cfg(target_vendor = "apple")]
fn source_link_preserves_the_requested_architecture() {
    use object::Object;

    let (arch, expected) = if cfg!(target_arch = "aarch64") {
        ("x86_64", object::Architecture::X86_64)
    } else {
        ("arm64", object::Architecture::Aarch64)
    };
    if !architecture_is_linkable(arch) {
        return;
    }

    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("main.c");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();
    let program = tmp.path().join("program");
    let output = rllvm("rllvm-cc")
        .args(["-arch", arch])
        .arg(&source)
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cross-architecture source link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read(&program).unwrap();
    let object = object::File::parse(&*data).unwrap();
    assert_eq!(object.architecture(), expected);

    let bitcode = tmp.path().join("program.bc");
    assert!(
        rllvm("rllvm-get-bc")
            .arg(&program)
            .arg("-o")
            .arg(&bitcode)
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(find_llvm_dis().unwrap())
        .arg(&bitcode)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(
        ir.contains(&format!("target triple = \"{arch}-apple-")),
        "bitcode target does not match {arch}: {ir}"
    );
}

/// The `save-temps` marker object is built for the link's target, not the
/// host.
///
/// A bare `clang -c` produces a host-native object, which a
/// cross-architecture link discards with a warning -- "ignoring file ...,
/// found architecture 'arm64', required architecture 'x86_64'". The link
/// still succeeds and the merged module is still collected, so the build
/// looks fine while the binary names nothing: issue #96's failure mode,
/// restored. Every other `save-temps` test builds for the host and cannot see
/// this.
#[test]
#[cfg(target_vendor = "apple")]
fn save_temps_marker_is_built_for_the_link_target() {
    let arch = if cfg!(target_arch = "aarch64") {
        "x86_64"
    } else {
        "arm64"
    };
    if !architecture_is_linkable(arch) {
        return;
    }

    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        let status = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-arch", arch, "-flto", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-arch", arch, "-flto", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    let output = link.output().expect("Failed to run rllvm-cc");
    assert!(
        output.status.success(),
        "the cross-architecture save-temps link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let extracted = tmp.path().join("prog.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-m")
        .arg("-o")
        .arg(&extracted)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(status.success(), "rllvm-get-bc found nothing in the binary");
    let manifest = fs::read_to_string(tmp.path().join("prog.bc.manifest")).unwrap();
    assert!(
        manifest.trim().ends_with("prog.rllvm.bc"),
        "the binary must name the merged module:\n{manifest}"
    );
}

/// A `save-temps` link that merged nothing is an error, and the error says
/// why.
///
/// `-flto` in `LDFLAGS` alone is not enough: objects compiled without it give
/// the LTO pipeline nothing to merge. By then the marker has already gone
/// into the link, so the binary names a `<output>.rllvm.bc` that was never
/// produced -- which is why this stays an error rather than a warning.
#[test]
fn save_temps_mode_reports_a_link_that_merged_nothing() {
    let tmp = TempDir::new().unwrap();
    let sources = write_lto_sources(tmp.path());

    let mut objects = vec![];
    for source in &sources {
        let object = source.with_extension("o");
        // No `-flto` here: ordinary objects.
        let status = rllvm("rllvm-cc")
            .env("RLLVM_LTO_MODE", "save-temps")
            .args(["--", "-c", "-o"])
            .arg(&object)
            .arg(source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed on {source:?}");
        objects.push(object);
    }

    let program = tmp.path().join("prog");
    let mut link = rllvm("rllvm-cc");
    link.env("RLLVM_LTO_MODE", "save-temps")
        .args(["--", "-flto", "-o"])
        .arg(&program);
    for object in &objects {
        link.arg(object);
    }
    let output = link.output().expect("Failed to run rllvm-cc");
    assert!(
        !output.status.success(),
        "a link that merged nothing must fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["not LTO bitcode", "LDFLAGS", "prog.rllvm.bc"] {
        assert!(
            stderr.contains(expected),
            "the error must name {expected:?}:\n{stderr}"
        );
    }
}

/// The dependency file must describe the object the user asked for.
///
/// The wrapper runs the user's command and then compiles the source again for
/// the `.bc`. Both honour `-MD`/`-MF`, so whichever runs last owns the file.
/// When the `.bc` compile won, the depfile named the `.bc`, `make` never
/// learned the object depended on the header, and editing a header rebuilt
/// nothing -- a silently stale object.
#[test]
fn dependency_file_names_the_object_and_its_headers() {
    let tmp = TempDir::new().unwrap();
    let header = tmp.path().join("dep.h");
    let source = tmp.path().join("dep.c");
    let object = tmp.path().join("dep.o");
    let depfile = tmp.path().join("dep.d");
    fs::write(&header, "#define VALUE 1\n").unwrap();
    fs::write(
        &source,
        "#include \"dep.h\"\nint dep_fn(void) { return VALUE; }\n",
    )
    .unwrap();

    let status = rllvm("rllvm-cc")
        .args(["--", "-MD", "-MF"])
        .arg(&depfile)
        .args(["-c", "-o"])
        .arg(&object)
        .arg(&source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc failed");

    let deps = fs::read_to_string(&depfile).expect("no dependency file written");
    let target = deps.split(':').next().unwrap_or_default().trim();
    assert!(
        target.ends_with("dep.o"),
        "the depfile must name the object as its target, not an rllvm artifact:\n{deps}"
    );
    assert!(
        deps.contains("dep.h"),
        "the header must appear as a prerequisite, or a header edit rebuilds nothing:\n{deps}"
    );
}

/// Link mode has a second writer of the same file.
///
/// Besides the `.bc` compile, `build_object_file` recompiles each source to a
/// hidden intermediate object with the user's arguments, so it clobbers the
/// dependency file too.
#[test]
fn link_mode_leaves_the_dependency_file_naming_the_object() {
    let tmp = TempDir::new().unwrap();
    let header = tmp.path().join("prog.h");
    let source = tmp.path().join("prog.c");
    let program = tmp.path().join("prog");
    let depfile = tmp.path().join("prog.d");
    fs::write(&header, "#define START 3\n").unwrap();
    fs::write(
        &source,
        "#include \"prog.h\"\nint main(void) { return START - 3; }\n",
    )
    .unwrap();

    let status = rllvm("rllvm-cc")
        .args(["--", "-MD", "-MF"])
        .arg(&depfile)
        .arg("-o")
        .arg(&program)
        .arg(&source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc link failed");

    let deps = fs::read_to_string(&depfile).expect("no dependency file written");
    assert!(
        !deps.contains(".bc"),
        "no rllvm artifact may appear as the depfile's target:\n{deps}"
    );
    assert!(
        deps.contains("prog.h"),
        "the header must appear as a prerequisite:\n{deps}"
    );
}

/// `-MD` must not suppress bitcode generation.
///
/// `-M` and `-MM` stop after preprocessing and emit dependencies instead of an
/// object, so skipping bitcode for them is right. `-MD` compiles normally and
/// writes the depfile as a side effect. Classifying it with the former made a
/// link-mode build carrying `-MD` produce a binary with no bitcode at all, and
/// the skip was quiet, so nothing said so.
#[test]
fn dependency_flags_do_not_suppress_bitcode_in_link_mode() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("mdlink.c");
    let program = tmp.path().join("mdlink");
    let depfile = tmp.path().join("mdlink.d");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();

    let status = rllvm("rllvm-cc")
        .args(["--", "-MD", "-MF"])
        .arg(&depfile)
        .arg("-o")
        .arg(&program)
        .arg(&source)
        .status()
        .expect("Failed to run rllvm-cc");
    assert!(status.success(), "rllvm-cc link failed");

    let bitcode = tmp.path().join("mdlink.bc");
    let status = rllvm("rllvm-get-bc")
        .arg(&program)
        .arg("-o")
        .arg(&bitcode)
        .status()
        .expect("Failed to run rllvm-get-bc");
    assert!(
        status.success(),
        "-MD must not suppress bitcode generation in link mode"
    );
    assert_bitcode_magic(&bitcode);
}

/// Writes a config with caching enabled and an isolated cache directory, so a
/// cache test never shares state with `~/.rllvm/cache` or with another test.
fn cache_config(dir: &Path) -> PathBuf {
    let base = fs::read_to_string(shared_config_path()).unwrap();
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let path = dir.join("cache-config.toml");
    fs::write(
        &path,
        format!(
            "{base}cache_enabled = true\ncache_dir = '{}'\n",
            cache.display()
        ),
    )
    .unwrap();
    path
}

/// Rebuild the same command and compare native execution with extracted IR.
fn assert_cached_value(dir: &Path, includes: &[&Path], cpath: Option<&Path>, value: i32) -> String {
    let object = dir.join("value.o");
    let depfile = dir.join("value.d");
    let mut command = rllvm("rllvm-cc");
    command
        .env("RLLVM_CONFIG", dir.join("cache-config.toml"))
        .env("RLLVM_CACHE", "1")
        .env_remove("CPATH")
        .arg("--rllvm-verbose=3")
        .args(["-c", "-MD", "-MF"])
        .arg(&depfile)
        .arg("-o")
        .arg(&object)
        .arg(dir.join("value.c"));
    for include in includes {
        command.arg("-I").arg(include);
    }
    if let Some(cpath) = cpath {
        command.env("CPATH", cpath);
    }
    let output = command.output().unwrap();
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{log}");
    let deps = fs::read_to_string(&depfile).unwrap();
    assert!(
        deps.starts_with(&format!("{}:", object.display())),
        "cache validation overwrote the user's dependency target: {deps}"
    );

    let executable = dir.join("value");
    let clang = find_llvm_dis().unwrap().with_file_name("clang");
    assert!(
        Command::new(clang)
            .arg(&object)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        Command::new(executable).status().unwrap().code(),
        Some(value)
    );

    let bitcode = dir.join("value.bc");
    assert!(
        rllvm("rllvm-get-bc")
            .arg(&object)
            .arg("-o")
            .arg(&bitcode)
            .status()
            .unwrap()
            .success()
    );
    let disassembled = Command::new(find_llvm_dis().unwrap())
        .arg(bitcode)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert!(disassembled.status.success());
    let ir = String::from_utf8_lossy(&disassembled.stdout);
    assert!(
        ir.contains(&format!("ret i32 {value}")),
        "native object returns {value}, but extracted IR is stale:\n{ir}\n{log}"
    );
    log
}

#[test]
fn cache_resolves_new_shadow_headers() {
    let tmp = TempDir::new().unwrap();
    cache_config(tmp.path());
    let high = tmp.path().join("high");
    let low = tmp.path().join("low");
    fs::create_dir(&high).unwrap();
    fs::create_dir(&low).unwrap();
    fs::write(low.join("pick.h"), "#define PICK 11\n").unwrap();
    fs::write(
        tmp.path().join("value.c"),
        "#include <pick.h>\nint main(void) { return PICK; }\n",
    )
    .unwrap();
    assert_cached_value(tmp.path(), &[&high, &low], None, 11);
    assert!(assert_cached_value(tmp.path(), &[&high, &low], None, 11).contains("Cache hit:"));
    fs::write(high.join("pick.h"), "#define PICK 22\n").unwrap();
    assert_cached_value(tmp.path(), &[&high, &low], None, 22);
}

#[test]
fn cache_resolves_current_include_environment() {
    let tmp = TempDir::new().unwrap();
    cache_config(tmp.path());
    let env_a = tmp.path().join("env_a");
    let env_b = tmp.path().join("env_b");
    fs::create_dir(&env_a).unwrap();
    fs::create_dir(&env_b).unwrap();
    fs::write(env_a.join("pick.h"), "#define PICK 31\n").unwrap();
    fs::write(env_b.join("pick.h"), "#define PICK 42\n").unwrap();
    fs::write(
        tmp.path().join("value.c"),
        "#include <pick.h>\nint main(void) { return PICK; }\n",
    )
    .unwrap();
    assert_cached_value(tmp.path(), &[], Some(&env_a), 31);
    assert!(assert_cached_value(tmp.path(), &[], Some(&env_a), 31).contains("Cache hit:"));
    assert_cached_value(tmp.path(), &[], Some(&env_b), 42);
}

#[test]
fn cache_resolves_previously_missing_has_include_headers() {
    let tmp = TempDir::new().unwrap();
    cache_config(tmp.path());
    fs::write(
        tmp.path().join("value.c"),
        "#if __has_include(<optional.h>)\n#define PICK 22\n#else\n#define PICK 11\n#endif\nint main(void) { return PICK; }\n",
    )
    .unwrap();
    assert_cached_value(tmp.path(), &[tmp.path()], None, 11);
    assert!(assert_cached_value(tmp.path(), &[tmp.path()], None, 11).contains("Cache hit:"));
    // The header is only probed, never included, so even a fresh depfile
    // has the same closure before and after its appearance.
    fs::write(tmp.path().join("optional.h"), "").unwrap();
    assert_cached_value(tmp.path(), &[tmp.path()], None, 22);
}

/// A cached `.bc` must not survive a change to a header it includes.
///
/// The object is always compiled fresh, so a stale cache entry makes the
/// binary and the bitcode extracted from it disagree silently -- the worst
/// failure this tool has. Note the build must be an ordinary `-c`: `-emit-llvm`
/// skips bitcode generation entirely, so it never reaches the cache.
#[test]
fn cache_tracks_changes_to_included_headers() {
    let tmp = TempDir::new().unwrap();
    let config = cache_config(tmp.path());
    let header = tmp.path().join("val.h");
    let source = tmp.path().join("cached.c");
    fs::write(&source, "#include \"val.h\"\nint f(void) { return VAL; }\n").unwrap();

    let build = |value: &str, tag: &str| -> Vec<u8> {
        fs::write(&header, format!("#define VAL {value}\n")).unwrap();
        let object = tmp.path().join(format!("cached_{tag}.o"));
        let status = Command::new(cargo_bin("rllvm-cc"))
            .env("RLLVM_CONFIG", &config)
            .args(["--", "-c", "-o"])
            .arg(&object)
            .arg(&source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed");

        let extracted = tmp.path().join(format!("cached_{tag}.bc"));
        let status = Command::new(cargo_bin("rllvm-get-bc"))
            .env("RLLVM_CONFIG", &config)
            .arg(&object)
            .arg("-o")
            .arg(&extracted)
            .status()
            .expect("Failed to run rllvm-get-bc");
        assert!(status.success(), "rllvm-get-bc failed");
        fs::read(&extracted).unwrap()
    };

    let first = build("111", "first");
    let second = build("222", "second");
    assert_ne!(
        first, second,
        "the cache served bitcode built against the old header"
    );
}

/// Include search order decides which header of a given name wins, so two
/// argument lists that differ only in that order are different compilations.
#[test]
fn cache_distinguishes_include_search_order() {
    let tmp = TempDir::new().unwrap();
    let config = cache_config(tmp.path());
    let dir_a = tmp.path().join("a");
    let dir_b = tmp.path().join("b");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_a.join("which.h"), "#define WHICH 1\n").unwrap();
    fs::write(dir_b.join("which.h"), "#define WHICH 2\n").unwrap();

    let source = tmp.path().join("order.c");
    fs::write(
        &source,
        "#include \"which.h\"\nint g(void) { return WHICH; }\n",
    )
    .unwrap();

    let build = |first: &Path, second: &Path, tag: &str| -> Vec<u8> {
        let object = tmp.path().join(format!("order_{tag}.o"));
        let status = Command::new(cargo_bin("rllvm-cc"))
            .env("RLLVM_CONFIG", &config)
            .arg("--")
            .arg("-I")
            .arg(first)
            .arg("-I")
            .arg(second)
            .args(["-c", "-o"])
            .arg(&object)
            .arg(&source)
            .status()
            .expect("Failed to run rllvm-cc");
        assert!(status.success(), "rllvm-cc failed");

        let extracted = tmp.path().join(format!("order_{tag}.bc"));
        let status = Command::new(cargo_bin("rllvm-get-bc"))
            .env("RLLVM_CONFIG", &config)
            .arg(&object)
            .arg("-o")
            .arg(&extracted)
            .status()
            .expect("Failed to run rllvm-get-bc");
        assert!(status.success(), "rllvm-get-bc failed");
        fs::read(&extracted).unwrap()
    };

    let a_first = build(&dir_a, &dir_b, "ab");
    let b_first = build(&dir_b, &dir_a, "ba");
    assert_ne!(
        a_first, b_first,
        "the cache key ignored include order, so the second build reused the first's bitcode"
    );
}
