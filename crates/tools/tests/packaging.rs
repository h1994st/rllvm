//! The release archives must list exactly the binaries a default build makes.
//!
//! `dist` ships every `[[bin]]` it finds in this package and builds it with
//! default features, so a binary behind `required-features` is enumerated and
//! then not found: the 0.5.1 release failed on `failed to find bin
//! rllvm-query`. #215 papered over that with a per-target
//! `[package.metadata.dist.binaries]` map; giving `rllvm-query` its own crate
//! removed the cause, and the map with it.
//!
//! Neither way of losing that shows up before a release runs -- a gated binary
//! breaks the build, and an override that drifts from `targets` silently ships
//! the wrong set for a target it omits -- so both are checked here.

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

#[test]
fn dist_can_build_every_binary_it_ships() {
    let cargo = manifest("crates/tools/Cargo.toml");

    let gated: BTreeSet<&str> = cargo["bin"]
        .as_array()
        .expect("[[bin]] entries")
        .iter()
        .filter(|bin| bin.get("required-features").is_some())
        .map(|bin| bin["name"].as_str().expect("bin name"))
        .collect();
    assert!(
        gated.is_empty(),
        "dist builds this package with default features and cannot build {gated:?}; \
         a binary that needs extra features belongs in its own crate"
    );

    // With no gated binary there is nothing to override, and no second list to
    // drift from `targets`: dist's own enumeration is the default build's.
    assert!(
        cargo
            .get("package")
            .and_then(|package| package.get("metadata"))
            .and_then(|metadata| metadata.get("dist"))
            .and_then(|dist| dist.get("binaries"))
            .is_none(),
        "[package.metadata.dist.binaries] is per target and falls back to every \
         binary for a target it omits; only a gated binary needs it"
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
