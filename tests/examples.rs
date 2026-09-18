//! Runs every example's `check.sh`, so the tutorials cannot rot unnoticed.
//!
//! `examples/cmake/README.md` documented `rllvm-get-bc build/hello` as
//! producing `build/hello.bc` for as long as the example existed. It never
//! did: the default output goes to the working directory. Nothing could catch
//! that, because nothing ran the examples.
//!
//! Discovery is by glob, so an example is covered the moment it ships a
//! script and no new example needs a change here.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

#[path = "common/toolchain.rs"]
mod toolchain;

/// The exit code an example uses to report a missing prerequisite, following
/// automake. Every other non-zero code is a failure.
///
/// A reserved code rather than a sentinel string: the harness never has to
/// parse output to decide an outcome.
const SKIP: i32 = 77;

/// The directory holding the binaries under test, so a script reaches those
/// rather than an rllvm the developer happens to have installed.
fn binary_directory() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_rllvm-cc"));
    path.pop();
    path
}

/// Every example that ships a `check.sh`, in a stable order.
fn examples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("check.sh").is_file())
        .collect();
    found.sort();
    found
}

/// Every path under `directory`, for the guard that an example writes only
/// where it is told to.
fn contents(directory: &Path) -> BTreeSet<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            seen.insert(path);
        }
    }
    seen
}

#[test]
fn every_example_verifies_itself() {
    let home = tempfile::tempdir().unwrap();
    let config = toolchain::scratch_rllvm_config(home.path());
    let llvm_config = toolchain::llvm_bin("llvm-config");
    let bindir = llvm_config.parent().expect("llvm-config sits in a bindir");
    let path = format!(
        "{}:{}",
        binary_directory().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut verified = 0;
    let mut failures: Vec<String> = Vec::new();

    for example in examples() {
        let name = example.file_name().unwrap().to_string_lossy().into_owned();
        let scratch = tempfile::tempdir().unwrap();
        let before = contents(&example);

        let output = Command::new(example.join("check.sh"))
            .arg(scratch.path())
            .current_dir(&example)
            .env("PATH", &path)
            .env("RLLVM_CONFIG", &config)
            .env("LLVM_BINDIR", bindir)
            .output()
            .unwrap_or_else(|error| panic!("{name}: cannot run check.sh: {error}"));

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Checked before the exit code: an example that passed its own
        // assertions while writing into the repository still fails.
        if contents(&example) != before {
            failures.push(format!(
                "{name}: wrote into its own directory instead of the one it was given"
            ));
            continue;
        }

        match output.status.code() {
            Some(0) => verified += 1,
            Some(SKIP) => eprintln!("skipping {name}: {stdout}"),
            code => failures.push(format!(
                "{name}: check.sh exited {code:?}\n\
                 --- stdout ---\n{stdout}\n\
                 --- stderr ---\n{}",
                String::from_utf8_lossy(&output.stderr)
            )),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        verified > 0,
        "every example skipped, so this run verified nothing"
    );
}
