//! Fixtures shared by the integration test binaries.
//!
//! Lives here rather than in one test file because source digests are a
//! catalog concern that exists without the `query` feature, while the status
//! they produce is only observable with it. Both halves have to assert
//! against the same program, or they are not testing the same thing.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::TempDir;

/// The id [`source_and_header`] pins on its module. `inventory` derives ids
/// from a content hash, which a test cannot predict.
pub const MODULE_ID: &str = "unit";

/// A tool from the configured LLVM's bindir.
pub fn llvm_bin(name: &str) -> PathBuf {
    let config = rllvm::utils::find_llvm_config().unwrap();
    let output = Command::new(config).arg("--bindir").output().unwrap();
    assert!(output.status.success());
    Path::new(String::from_utf8(output.stdout).unwrap().trim()).join(name)
}

/// Writes `source` into the scratch directory and compiles it to bitcode
/// beside itself, with the flags the catalog fixtures want.
pub fn compile_bitcode(scratch: &TempDir, name: &str, source: &str) -> PathBuf {
    let source_path = scratch.path().join(name);
    std::fs::write(&source_path, source).unwrap();
    compile_bitcode_file(&source_path, &["-g", "-O0"])
}

/// Compiles a source already on disk, for a fixture that puts a header beside
/// it first or chooses its own debug format.
pub fn compile_bitcode_file(source: &Path, flags: &[&str]) -> PathBuf {
    let module = source.with_extension("bc");
    let status = Command::new(llvm_bin("clang"))
        .args(flags)
        .args(["-emit-llvm", "-c"])
        .arg(source)
        .arg("-o")
        .arg(&module)
        .status()
        .unwrap();
    assert!(status.success(), "clang failed on {}", source.display());
    module
}

/// Writes a catalog straight to `path`. Not `catalog::write_catalog`, whose
/// no-clobber publish these fixtures do not need.
pub fn write_catalog_json(path: &Path, catalog: &rllvm::catalog::ModuleCatalog) {
    std::fs::write(path, serde_json::to_vec_pretty(catalog).unwrap()).unwrap();
}

/// A translation unit that includes a header, compiled and inventoried the
/// way `rllvm-get-bc` does.
///
/// Built around a header on purpose. The header is the case that used to be
/// impossible: nothing associated it, so a location inside it could only ever
/// answer `unknown`, and editing it -- the ordinary way one C change
/// invalidates line numbers across many modules -- went unnoticed.
pub struct SourceFixture {
    pub catalog: PathBuf,
    pub source: PathBuf,
    pub header: PathBuf,
}

impl SourceFixture {
    /// The digest recorded for one of the two files, from whichever
    /// association carries one -- the same rule the loader follows.
    pub fn digest(&self, file: &Path) -> Option<rllvm::catalog::SourceDigest> {
        let catalog = rllvm::catalog::read_catalog(&self.catalog).unwrap();
        let mut associated = catalog.modules[0]
            .sources
            .iter()
            .filter(|association| association.resolved_path() == file)
            .peekable();
        assert!(
            associated.peek().is_some(),
            "no association recorded for {}",
            file.display()
        );
        associated.find_map(|association| association.digest.clone())
    }
}

pub fn source_and_header(scratch: &TempDir) -> SourceFixture {
    let header = scratch.path().join("h.h");
    std::fs::write(&header, "int helper(int x){return x+1;}\n").unwrap();
    let module = compile_bitcode(
        scratch,
        "unit.c",
        "#include \"h.h\"\nint main(void){return helper(2);}\n",
    );
    let mut catalog =
        rllvm::catalog::inventory(&module, scratch.path(), Some(&llvm_bin("llvm-dis"))).unwrap();
    catalog.modules[0].id = MODULE_ID.to_string();
    let path = scratch.path().join("catalog.json");
    write_catalog_json(&path, &catalog);
    SourceFixture {
        catalog: path,
        source: scratch.path().join("unit.c"),
        header,
    }
}
