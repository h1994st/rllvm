//! The release archives must list exactly the binaries a default build makes.
//!
//! `dist` ships every `[[bin]]` it finds, which includes `rllvm-query` --
//! gated behind `required-features = ["query"]` and therefore absent from the
//! default build `dist` performs. The 0.5.1 release failed on `failed to find
//! bin rllvm-query` after `rllvm-query` was added.
//!
//! `[package.metadata.dist.binaries]` overrides that list, but only per
//! target, and both ways of getting it wrong are quiet: a target missing from
//! the map falls back to every binary, which breaks the release, and a binary
//! missing from a list is simply never shipped. Neither shows up until a
//! release runs, so they are checked here instead.

use std::{collections::BTreeSet, fs, path::Path};

use toml::Value;

fn manifest(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .parse::<Value>()
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

/// The binaries a build without extra features produces.
fn default_binaries(cargo: &Value) -> BTreeSet<String> {
    cargo["bin"]
        .as_array()
        .expect("[[bin]] entries")
        .iter()
        .filter(|bin| bin.get("required-features").is_none())
        .map(|bin| bin["name"].as_str().expect("bin name").to_owned())
        .collect()
}

#[test]
fn dist_ships_the_default_binaries_on_every_target() {
    let cargo = manifest("Cargo.toml");
    let workspace = manifest("dist-workspace.toml");

    let expected = default_binaries(&cargo);
    assert!(
        !expected.contains("rllvm-query"),
        "rllvm-query must stay feature-gated; dist cannot build it"
    );

    let targets: BTreeSet<String> = workspace["dist"]["targets"]
        .as_array()
        .expect("dist targets")
        .iter()
        .map(|target| target.as_str().expect("target triple").to_owned())
        .collect();

    let binaries = cargo["package"]["metadata"]["dist"]["binaries"]
        .as_table()
        .expect("[package.metadata.dist.binaries]");

    let listed: BTreeSet<String> = binaries.keys().cloned().collect();
    assert_eq!(
        listed, targets,
        "every dist target needs its own binary list, and only real targets \
         count: an unrecognised key is accepted and ignored"
    );

    for (target, names) in binaries {
        let shipped: BTreeSet<String> = names
            .as_array()
            .unwrap_or_else(|| panic!("{target} must list binaries"))
            .iter()
            .map(|name| name.as_str().expect("binary name").to_owned())
            .collect();
        assert_eq!(shipped, expected, "wrong binaries shipped for {target}");
    }
}
