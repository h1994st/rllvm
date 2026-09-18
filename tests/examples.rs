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
    fs::File,
    io,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant, SystemTime},
};

#[path = "common/toolchain.rs"]
mod toolchain;

/// The exit code an example uses to report a missing prerequisite, following
/// automake. Every other non-zero code is a failure.
///
/// A reserved code rather than a sentinel string: the harness never has to
/// parse output to decide an outcome.
const SKIP: i32 = 77;

/// How long one example may run before the harness kills it.
///
/// Far above any legitimate example -- the three in the tree finish in under a
/// second, and a cold autotools or Cargo build is minutes at worst -- and far
/// below a CI job limit, so a hang is reported as one example failing rather
/// than as the whole job timing out with nothing to point at.
const TIMEOUT: Duration = Duration::from_secs(300);

/// How often the harness checks whether the child has finished.
const POLL: Duration = Duration::from_millis(50);

/// What one capped run produced. `status` is `None` when the child outstayed
/// its timeout and was killed.
struct Capped {
    status: Option<ExitStatus>,
    stdout: String,
    stderr: String,
}

/// Runs `command` to completion, killing it if it outstays `timeout`.
///
/// Child output goes to files rather than pipes. A polled wait cannot also
/// drain a pipe, so a child that filled the buffer would block forever --
/// exactly the hang this function exists to end.
///
/// On expiry the signal goes to the child's process group, not just the child:
/// a watchdog that kills only the script leaves the hung build beneath it
/// running.
fn run_capped(command: &mut Command, capture: &Path, timeout: Duration) -> io::Result<Capped> {
    let out_path = capture.join("stdout");
    let err_path = capture.join("stderr");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(File::create(&out_path)?)
        .stderr(File::create(&err_path)?)
        .process_group(0)
        .spawn()?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            // Negative pid: the group, so the child's own children die too.
            // std cannot signal a group without libc, and the alternative is
            // orphaning the process that is actually stuck.
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(format!("-{}", child.id()))
                .status();
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(POLL);
    };

    Ok(Capped {
        status,
        stdout: std::fs::read_to_string(&out_path).unwrap_or_default(),
        stderr: std::fs::read_to_string(&err_path).unwrap_or_default(),
    })
}

/// What became of one example. The harness records one per example so a CI
/// reader can see which ran, rather than inferring it from a green tick.
enum Outcome {
    Verified,
    Skipped(String),
    Failed,
}

/// Renders the per-example record CI prints.
///
/// libtest captures a passing test's output, so a run that quietly degrades
/// from three examples verified to one looks exactly like a healthy one. This
/// is the record that makes the difference visible.
fn summary(outcomes: &[(String, Outcome)]) -> String {
    let mut lines = String::new();
    let (mut verified, mut skipped, mut failed) = (0, 0, 0);
    for (name, outcome) in outcomes {
        match outcome {
            Outcome::Verified => {
                verified += 1;
                lines.push_str(&format!("verified  {name}\n"));
            }
            Outcome::Skipped(reason) => {
                skipped += 1;
                lines.push_str(&format!("skipped   {name}  {reason}\n"));
            }
            Outcome::Failed => {
                failed += 1;
                lines.push_str(&format!("failed    {name}\n"));
            }
        }
    }
    lines.push_str(&format!(
        "{verified} verified, {skipped} skipped, {failed} failed\n"
    ));
    lines
}

/// Where the record is written, for CI to print. `CARGO_TARGET_TMPDIR` is
/// `<target>/tmp`, so the path is predictable without hardcoding `target/`.
fn summary_path() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("examples-summary.txt")
}

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
    let mut outcomes: Vec<(String, Outcome)> = Vec::new();

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

        let mut command = Command::new(example.join("check.sh"));
        command
            .arg(scratch.path())
            .current_dir(&example)
            .env("PATH", &path)
            .env("RLLVM_CONFIG", &config)
            .env("RLLVM_CACHE", "0")
            .env_remove("RLLVM_BITCODE_ROOT")
            .env_remove("RLLVM_LTO_MODE")
            .env_remove("RLLVM_LOG_LEVEL")
            .env("LLVM_BINDIR", bindir);

        // A non-executable or unspawnable check.sh is a failure of that one
        // example, not a reason to abort the rest.
        let capped = match run_capped(&mut command, home.path(), TIMEOUT) {
            Ok(capped) => capped,
            Err(error) => {
                failures.push(format!("{name}: cannot run check.sh: {error}"));
                outcomes.push((name, Outcome::Failed));
                continue;
            }
        };

        let stdout = capped.stdout.trim().to_string();

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
            outcomes.push((name, Outcome::Failed));
            continue;
        }

        match capped.status {
            None => {
                outcomes.push((name.clone(), Outcome::Failed));
                failures.push(format!(
                    "{name}: still running after {}s, killed\n\
                 --- stdout ---\n{stdout}\n\
                 --- stderr ---\n{}",
                    TIMEOUT.as_secs(),
                    capped.stderr
                ));
            }
            Some(status) => match status.code() {
                Some(0) => {
                    verified += 1;
                    outcomes.push((name, Outcome::Verified));
                }
                Some(SKIP) => {
                    eprintln!("skipping {name}: {stdout}");
                    outcomes.push((name, Outcome::Skipped(stdout)));
                }
                code => {
                    failures.push(format!(
                        "{name}: check.sh exited {code:?}\n\
                         --- stdout ---\n{stdout}\n\
                         --- stderr ---\n{}",
                        capped.stderr
                    ));
                    outcomes.push((name, Outcome::Failed));
                }
            },
        }
    }

    // Written before the assertions: a failing run is exactly when a reader
    // most wants to know what did and did not run.
    let record = summary(&outcomes);
    let path = summary_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &record).unwrap();
    eprintln!("{record}");

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        verified > 0,
        "every example skipped, so this run verified nothing"
    );
}

/// A script that never finishes must be killed and reported, not left to hang
/// the whole suite until the CI job limit.
///
/// The proof is the clock: `sleep 30` under a 200ms cap returns immediately.
/// If the timeout did not fire, this test would take thirty seconds.
#[test]
fn a_command_that_outstays_its_timeout_is_killed() {
    let capture = tempfile::tempdir().unwrap();
    let started = Instant::now();

    let mut command = Command::new("sleep");
    command.arg("30");
    let capped = run_capped(&mut command, capture.path(), Duration::from_millis(200)).unwrap();

    assert!(
        capped.status.is_none(),
        "a killed command must report no exit status"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the cap must end the wait, not the command: took {:?}",
        started.elapsed()
    );
}

/// The record must name every example and its fate, so a reader can tell a
/// run that verified everything from one that skipped most of it. A count
/// alone would not: "1 verified" reads the same whether two examples were
/// skipped or never existed.
#[test]
fn the_summary_names_every_example_and_its_fate() {
    let outcomes = vec![
        ("cmake".to_string(), Outcome::Verified),
        (
            "objc".to_string(),
            Outcome::Skipped("needs Darwin, this host is Linux".to_string()),
        ),
        ("wasm".to_string(), Outcome::Failed),
    ];

    let record = summary(&outcomes);

    assert_eq!(
        record,
        "verified  cmake\n\
         skipped   objc  needs Darwin, this host is Linux\n\
         failed    wasm\n\
         1 verified, 1 skipped, 1 failed\n"
    );
}
