//! Argument classification and rewriting for the rustc wrapper.
//!
//! Kept free of I/O so the cargo-shaped command lines that broke #85 can be
//! asserted on directly, without spawning rustc.

use std::path::{Path, PathBuf};

use crate::error::Error;

/// What rllvm has to do for one rustc invocation.
///
/// The two are not exclusive: `--crate-type lib --crate-type cdylib` both
/// links and archives, and then both steps run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Actions {
    /// rustc invokes a linker, so a marker object can ride in on `-C link-arg`.
    pub links: bool,
    /// rustc writes an archive itself, whose members have to be patched.
    pub archives: bool,
}

/// Crate types that make rustc invoke a linker.
const LINKING_CRATE_TYPES: [&str; 3] = ["bin", "cdylib", "dylib"];
/// Crate types that make rustc write an archive itself.
const ARCHIVING_CRATE_TYPES: [&str; 3] = ["lib", "rlib", "staticlib"];

/// Value of `--flag value` or `--flag=value`.
pub(crate) fn flag_value<'a>(args: &[&'a str], flag: &str) -> Option<&'a str> {
    let prefix = format!("{flag}=");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(prefix.as_str()) {
            return Some(value);
        }
        if *arg == flag {
            return iter.next().copied();
        }
    }
    None
}

/// Value of `-C key=value` or `-Ckey=value`.
pub(crate) fn codegen_value<'a>(args: &[&'a str], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let body = match arg.strip_prefix("-C") {
            Some("") => *iter.next()?,
            Some(rest) => rest,
            None => continue,
        };
        if let Some(value) = body.strip_prefix(prefix.as_str()) {
            return Some(value);
        }
    }
    None
}

/// Every `--crate-type` value, in either spelling. rustc accepts the flag more
/// than once, and accepts a comma-separated list in one.
fn crate_types<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut types = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--crate-type=") {
            types.extend(value.split(','));
        } else if *arg == "--crate-type"
            && let Some(value) = iter.next()
        {
            types.extend(value.split(','));
        }
    }
    types
}

/// Decide what to do with an invocation. `None` means pass it through.
pub(crate) fn classify(args: &[&str]) -> Option<Actions> {
    // Query invocations produce no artifact.
    if args
        .iter()
        .any(|a| matches!(*a, "--version" | "-V" | "-vV") || a.starts_with("--print"))
    {
        tracing::debug!("rustc: query invocation, passing through");
        return None;
    }

    // `cargo check` emits metadata and never codegens.
    if let Some(emit) = flag_value(args, "--emit") {
        let produces_code = emit.split(',').any(|e| {
            matches!(e, "link" | "obj") || e.starts_with("link=") || e.starts_with("obj=")
        });
        if !produces_code {
            tracing::debug!("rustc: --emit produces no code, passing through");
            return None;
        }
    }

    let types = crate_types(args);

    // A proc macro is a host dylib; its bitcode is of no use to the target.
    if types
        .iter()
        .any(|t| *t == "proc-macro" || *t == "proc_macro")
    {
        tracing::debug!("rustc: proc-macro crate, passing through");
        return None;
    }

    // No `--crate-type` means bin, which links.
    if types.is_empty() {
        return Some(Actions {
            links: true,
            archives: false,
        });
    }

    Some(Actions {
        links: types.iter().any(|t| LINKING_CRATE_TYPES.contains(t)),
        archives: types.iter().any(|t| ARCHIVING_CRATE_TYPES.contains(t)),
    })
}

/// Where this crate's bitcode goes.
///
/// Cargo never passes `-o`; it passes `--out-dir` with `--crate-name` and
/// `-C extra-filename`. Failing to model that is #85.
pub(crate) fn bitcode_path(args: &[&str], store: Option<&Path>) -> Result<PathBuf, Error> {
    let derived = if let Some(output) = flag_value(args, "-o") {
        PathBuf::from(output).with_extension("bc")
    } else {
        let out_dir = flag_value(args, "--out-dir").ok_or_else(|| {
            Error::InvalidArguments(
                "rustc invocation has neither -o nor --out-dir, so no bitcode path can be derived"
                    .into(),
            )
        })?;
        let crate_name = flag_value(args, "--crate-name").ok_or_else(|| {
            Error::InvalidArguments("rustc invocation has --out-dir but no --crate-name".into())
        })?;
        let extra = codegen_value(args, "extra-filename").unwrap_or("");
        PathBuf::from(out_dir).join(format!("{crate_name}{extra}.bc"))
    };

    let Some(store) = store else {
        return Ok(derived);
    };
    let filename = derived.file_name().ok_or_else(|| {
        Error::InvalidArguments(format!("derived bitcode path {derived:?} has no file name"))
    })?;
    Ok(store.join(filename))
}

/// Add `llvm-bc=<path>` to `--emit` so one rustc run produces both outputs.
pub(crate) fn rewrite_emit(args: &[String], bitcode: &Path) -> Vec<String> {
    let request = format!("llvm-bc={}", bitcode.display());
    let mut rewritten = Vec::with_capacity(args.len() + 1);
    let mut found = false;
    let mut iter = args.iter();

    while let Some(arg) = iter.next() {
        if let Some(list) = arg.strip_prefix("--emit=") {
            rewritten.push(format!("--emit={list},{request}"));
            found = true;
        } else if arg == "--emit"
            && let Some(list) = iter.next()
        {
            rewritten.push(format!("--emit={list},{request}"));
            found = true;
        } else {
            rewritten.push(arg.clone());
        }
    }

    if !found {
        // Naming any --emit value suppresses the default `link`.
        rewritten.push(format!("--emit=link,{request}"));
    }

    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape cargo uses for a library. #85: no `-o` anywhere.
    fn cargo_lib_args() -> Vec<&'static str> {
        vec![
            "--crate-name",
            "mylib",
            "--edition=2024",
            "src/lib.rs",
            "--crate-type",
            "lib",
            "--emit=dep-info,metadata,link",
            "-C",
            "embed-bitcode=no",
            "-C",
            "metadata=ea7a1b98699261a9",
            "-C",
            "extra-filename=-cc13993b960f3eb8",
            "--out-dir",
            "/w/target/debug/deps",
        ]
    }

    fn cargo_bin_args() -> Vec<&'static str> {
        vec![
            "--crate-name",
            "myapp",
            "src/main.rs",
            "--crate-type",
            "bin",
            "--emit=dep-info,link",
            "-C",
            "extra-filename=-1be80deae7695c72",
            "--out-dir",
            "/w/target/debug/deps",
        ]
    }

    #[test]
    fn classifies_cargo_lib_as_archiving() {
        let actions = classify(&cargo_lib_args()).expect("lib must not be skipped");
        assert!(actions.archives);
        assert!(!actions.links);
    }

    #[test]
    fn classifies_cargo_bin_as_linking() {
        let actions = classify(&cargo_bin_args()).expect("bin must not be skipped");
        assert!(actions.links);
        assert!(!actions.archives);
    }

    #[test]
    fn classifies_multiple_crate_types_as_both() {
        let args = vec![
            "--crate-name",
            "both",
            "src/lib.rs",
            "--crate-type",
            "lib",
            "--crate-type",
            "cdylib",
            "--emit=link",
            "--out-dir",
            "/w",
        ];
        let actions = classify(&args).expect("must not be skipped");
        assert!(actions.links && actions.archives);
    }

    #[test]
    fn classifies_absent_crate_type_as_linking() {
        let args = vec!["src/main.rs", "--out-dir", "/w", "--crate-name", "a"];
        assert!(classify(&args).expect("default is bin").links);
    }

    #[test]
    fn skips_query_and_check_and_proc_macro_invocations() {
        assert!(classify(&["--version"]).is_none());
        assert!(classify(&["-vV"]).is_none());
        assert!(classify(&["--print", "cfg"]).is_none());
        // `cargo check`: metadata without link or obj.
        assert!(classify(&["src/lib.rs", "--emit=dep-info,metadata", "--out-dir", "/w"]).is_none());
        assert!(
            classify(&[
                "src/lib.rs",
                "--crate-type",
                "proc-macro",
                "--emit=link",
                "--out-dir",
                "/w"
            ])
            .is_none()
        );
    }

    #[test]
    fn derives_bitcode_path_from_out_dir_when_there_is_no_output_flag() {
        let path = bitcode_path(&cargo_lib_args(), None).expect("derivable");
        assert_eq!(
            path,
            PathBuf::from("/w/target/debug/deps/mylib-cc13993b960f3eb8.bc")
        );
    }

    #[test]
    fn derives_bitcode_path_from_output_flag_when_present() {
        let args = vec!["src/main.rs", "-o", "/w/prog"];
        assert_eq!(
            bitcode_path(&args, None).unwrap(),
            PathBuf::from("/w/prog.bc")
        );
    }

    #[test]
    fn bitcode_store_path_overrides_the_directory() {
        let store = PathBuf::from("/store");
        let path = bitcode_path(&cargo_lib_args(), Some(&store)).unwrap();
        assert_eq!(path, PathBuf::from("/store/mylib-cc13993b960f3eb8.bc"));
    }

    /// A silent `Ok` is what hid #85 for as long as it hid.
    #[test]
    fn undeterminable_bitcode_path_is_an_error() {
        assert!(bitcode_path(&["src/lib.rs"], None).is_err());
    }

    #[test]
    fn rewrite_emit_appends_to_an_existing_list() {
        let args: Vec<String> = cargo_lib_args().iter().map(|s| s.to_string()).collect();
        let out = rewrite_emit(&args, Path::new("/w/mylib.bc"));
        assert!(out.contains(&"--emit=dep-info,metadata,link,llvm-bc=/w/mylib.bc".to_string()));
        assert_eq!(out.iter().filter(|a| a.starts_with("--emit")).count(), 1);
    }

    /// Naming any `--emit` value suppresses the default `link`, so the
    /// artifact would vanish if `link` were not added back.
    #[test]
    fn rewrite_emit_adds_link_when_emit_was_absent() {
        let args = vec![
            "src/main.rs".to_string(),
            "-o".to_string(),
            "/w/p".to_string(),
        ];
        let out = rewrite_emit(&args, Path::new("/w/p.bc"));
        assert!(out.contains(&"--emit=link,llvm-bc=/w/p.bc".to_string()));
    }

    #[test]
    fn reads_flag_and_codegen_values_in_both_spellings() {
        assert_eq!(flag_value(&["--out-dir", "/w"], "--out-dir"), Some("/w"));
        assert_eq!(flag_value(&["--out-dir=/w"], "--out-dir"), Some("/w"));
        assert_eq!(flag_value(&["--out-dir"], "--out-dir"), None);
        assert_eq!(
            codegen_value(&["-C", "extra-filename=-abc"], "extra-filename"),
            Some("-abc")
        );
        assert_eq!(
            codegen_value(&["-Cextra-filename=-abc"], "extra-filename"),
            Some("-abc")
        );
        assert_eq!(
            codegen_value(&["-C", "opt-level=3"], "extra-filename"),
            None
        );
    }
}
