//! Runs every example's `check.sh`, so the tutorials cannot rot unnoticed.
//!
//! `examples/cmake/README.md` documented `rllvm-get-bc build/hello` as
//! producing `build/hello.bc` for as long as the example existed. It never
//! did: the default output goes to the working directory. Nothing could catch
//! that, because nothing ran the examples.
//!
//! Discovery is by glob, so an example is covered the moment it ships a
//! script; a directory that forgets one is still caught, by name, below.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
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

/// `examples/`, the directory every example lives under.
fn examples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// Every directory directly under `examples/`, whether or not it ships a
/// `check.sh`.
fn example_directories() -> Vec<PathBuf> {
    let root = examples_root();
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    found
}

/// Every example that ships a `check.sh`, in a stable order.
fn examples() -> Vec<PathBuf> {
    example_directories()
        .into_iter()
        .filter(|path| path.join("check.sh").is_file())
        .collect()
}

/// A size-and-mtime fingerprint of every path under `directory`, for the
/// guard that an example writes only where it is told to.
fn snapshot(directory: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut seen = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            let modified = metadata
                .modified()
                .unwrap_or_else(|error| panic!("no mtime for {}: {error}", path.display()));
            seen.insert(path, (metadata.len(), modified));
        }
    }
    seen
}

/// Paths created, mutated, or removed between two snapshots.
fn changed_paths(
    before: &BTreeMap<PathBuf, (u64, SystemTime)>,
    after: &BTreeMap<PathBuf, (u64, SystemTime)>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, stamp) in after {
        if before.get(path) != Some(stamp) {
            changed.push(path.display().to_string());
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            changed.push(format!("{} (removed)", path.display()));
        }
    }
    changed.sort();
    changed
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

    let root = examples_root();
    let mut verified = 0;
    let mut failures: Vec<String> = Vec::new();

    let missing: Vec<String> = example_directories()
        .into_iter()
        .filter(|path| !path.join("check.sh").is_file())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    if !missing.is_empty() {
        failures.push(format!("missing check.sh in: {}", missing.join(", ")));
    }

    // Snapshotted once over all of examples/, then refreshed after each run,
    // so a write into a sibling example or examples/ itself is caught too,
    // not just a write into the example currently running.
    let mut before = snapshot(&root);

    for example in examples() {
        let name = example.file_name().unwrap().to_string_lossy().into_owned();
        let scratch = tempfile::tempdir().unwrap();

        let spawned = Command::new(example.join("check.sh"))
            .arg(scratch.path())
            .current_dir(&example)
            .env("PATH", &path)
            .env("RLLVM_CONFIG", &config)
            .env("RLLVM_CACHE", "0")
            .env_remove("RLLVM_BITCODE_ROOT")
            .env_remove("RLLVM_LTO_MODE")
            .env_remove("RLLVM_LOG_LEVEL")
            .env("LLVM_BINDIR", bindir)
            .output();

        // A non-executable or unspawnable check.sh is a failure of that one
        // example, not a reason to abort the rest.
        let output = match spawned {
            Ok(output) => output,
            Err(error) => {
                failures.push(format!("{name}: cannot run check.sh: {error}"));
                continue;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Checked before the exit code: an example that passed its own
        // assertions while writing outside the directory it was given still
        // fails, whether that write landed in its own directory, a sibling
        // example, or examples/ itself.
        let after = snapshot(&root);
        let changed = changed_paths(&before, &after);
        before = after;
        if !changed.is_empty() {
            failures.push(format!(
                "{name}: wrote outside the directory it was given: {}",
                changed.join(", ")
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
