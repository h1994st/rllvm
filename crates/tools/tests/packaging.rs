//! The release archives must list exactly the binaries a default build makes.
//!
//! `dist` ships every `[[bin]]` it finds in a distributed package and builds it
//! with default features, so a binary behind `required-features` is enumerated
//! and then not found: the 0.5.1 release failed on `failed to find bin
//! rllvm-query`. #215 papered over that with a per-target
//! `[package.metadata.dist.binaries]` map; giving `rllvm-query` its own crate
//! removed the cause, and the map with it.
//!
//! Neither way of losing that shows up before a release runs -- a gated binary
//! breaks the build, and an override that drifts from `targets` silently ships
//! the wrong set for a target it omits -- so both are checked here, along with
//! which crates dist announces at all and which targets it announces them for.

use std::{collections::BTreeSet, fs, path::Path};

use toml::Value;

fn manifest(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .parse::<Value>()
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

/// Whether dist announces this package as an app.
///
/// The opt-out is `[package.metadata.dist] dist = false`; absent, dist decides
/// for itself, which is what every app relies on.
fn distributed(cargo: &Value) -> bool {
    cargo
        .get("package")
        .and_then(|package| package.get("metadata"))
        .and_then(|metadata| metadata.get("dist"))
        .and_then(|dist| dist.get("dist"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The crates whose binaries dist ships, and so builds without features.
const APP_MANIFESTS: [&str; 2] = ["crates/tools/Cargo.toml", "crates/query/Cargo.toml"];

#[test]
fn dist_can_build_every_binary_it_ships() {
    for path in APP_MANIFESTS {
        let cargo = manifest(path);

        let gated: BTreeSet<&str> = cargo["bin"]
            .as_array()
            .expect("[[bin]] entries")
            .iter()
            .filter(|bin| bin.get("required-features").is_some())
            .map(|bin| bin["name"].as_str().expect("bin name"))
            .collect();
        assert!(
            gated.is_empty(),
            "dist builds {path} with default features and cannot build {gated:?}; \
             a binary that needs extra features belongs in its own crate"
        );

        // With no gated binary there is nothing to override, and no second list
        // to drift from `targets`: dist's own enumeration is the default
        // build's.
        assert!(
            cargo
                .get("package")
                .and_then(|package| package.get("metadata"))
                .and_then(|metadata| metadata.get("dist"))
                .and_then(|dist| dist.get("binaries"))
                .is_none(),
            "{path} sets [package.metadata.dist.binaries], which is per target and \
             falls back to every binary for a target it omits; only a gated binary \
             needs it"
        );
    }
}

/// dist announces a crate only when that crate has binaries to ship.
///
/// One announcement is one GitHub Release with its own archives, installer and
/// `source.tar.gz`. `rllvm-core` is a library: announced, it would publish an
/// archive holding no binary and an installer that installs nothing, beside the
/// two releases that do ship something. It reaches users through crates.io, from
/// the same release's publish job.
///
/// Nothing reports this either way. dist skips a package with no `[[bin]]`, so
/// the opt-out looks redundant right up to the day `rllvm-core` grows a binary
/// -- and then a library starts shipping one. The two apps are asserted from
/// the same table because the failure is symmetric: an app that acquires
/// `dist = false` stops being released, with no error anywhere.
#[test]
fn only_the_crates_with_binaries_are_dist_apps() {
    for (path, expected) in [
        ("crates/core/Cargo.toml", false),
        ("crates/tools/Cargo.toml", true),
        ("crates/query/Cargo.toml", true),
    ] {
        assert_eq!(
            distributed(&manifest(path)),
            expected,
            "{path} is {} by dist, which is not what the release expects",
            if expected { "skipped" } else { "announced" }
        );
    }
}

/// Every released target has to be one `rllvm-query` can link LLVM for.
///
/// `targets` is one list, shared by every app, so a triple added for `rllvm`
/// is demanded of `rllvm-query` too -- and that crate links LLVM statically
/// through llvm-sys. musl has no LLVM to link against, so the triple cannot
/// build, and the failure lands in a release build job after the other targets
/// have already been built and uploaded.
#[test]
fn released_targets_can_link_llvm() {
    let workspace = manifest("dist-workspace.toml");
    let targets: BTreeSet<&str> = workspace["dist"]["targets"]
        .as_array()
        .expect("dist targets")
        .iter()
        .map(|target| target.as_str().expect("target triple"))
        .collect();

    assert!(
        !targets.contains("x86_64-unknown-linux-musl"),
        "musl cannot link LLVM, so rllvm-query cannot ship for it: {targets:?}"
    );
}

/// An unpublished crate stays out of the release-please workspace.
///
/// release-please's cargo-workspace plugin walks the dependency graph from the
/// packages being released and version-bumps everything that depends on them.
/// `rllvm-testkit` depends on `rllvm-core`, so it would be bumped on every
/// release and drift away from the crates it is built alongside -- for a crate
/// that is never published and whose version nothing resolves.
///
/// Nothing in release-please's configuration exempts a dependent. What keeps it
/// out is this array: the plugin reads `workspace.members` textually over the
/// GitHub API and never runs `cargo metadata`, while cargo itself still treats
/// the crate as a member because it is a path dependency of members. Adding it
/// back here would silently resume the bumping, so the omission is asserted
/// rather than left to a comment.
#[test]
fn unpublished_crates_stay_out_of_the_release_workspace() {
    let root = manifest("Cargo.toml");
    let members: BTreeSet<&str> = root["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|m| m.as_str().expect("member path"))
        .collect();

    let testkit = manifest("crates/testkit/Cargo.toml");
    assert_eq!(
        testkit["package"]["publish"].as_bool(),
        Some(false),
        "this test only reasons about crates that are never published"
    );
    assert_eq!(
        testkit["package"]["version"].as_str(),
        Some("0.0.0"),
        "an unreleased crate carries the conventional placeholder version"
    );
    assert!(
        !members.contains("crates/testkit"),
        "crates/testkit is listed in workspace.members, so release-please will \
         version-bump it on every release: {members:?}"
    );
}

/// Each announcement builds only the package it ships.
///
/// dist's default is one `cargo build --workspace` per announcement, which
/// compiles every member whatever the tag selected. The `rllvm-v0.6.0` release
/// therefore built `crates/query`, and llvm-sys found no LLVM: the build setup
/// installs one only for an `rllvm-query` announcement, because Homebrew's
/// unprefixed `ar`, `nm` and `ranlib` would otherwise shadow the system tools
/// in a job that wants them. dist reported the consequence rather than the
/// cause -- `failed to find bin rllvm-cc` -- after all three targets had failed.
///
/// `precise-builds` turns that into `cargo build --package=rllvm`, so the two
/// apps stop sharing a build and the LLVM setup can stay conditional. Only a
/// release exercises this, and dist force-enables the same flag when packages
/// disagree about features, so the day someone adds a feature this passes for a
/// reason that has nothing to do with the setting being written down here.
#[test]
fn each_app_builds_only_its_own_package() {
    let workspace = manifest("dist-workspace.toml");

    assert_eq!(
        workspace["dist"]
            .get("precise-builds")
            .and_then(Value::as_bool),
        Some(true),
        "without precise-builds dist builds the whole workspace for every \
         announcement, so an rllvm release compiles rllvm-query and links LLVM"
    );
}
