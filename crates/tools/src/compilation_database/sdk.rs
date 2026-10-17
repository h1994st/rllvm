//! Optional macOS SDK discovery for imported compilations.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
};

use rllvm_core::constants::{arg_exact_match_map, arg_patterns};

/// Stops a clang built with `CLANG_USE_XCSELECT` from finding an SDK through
/// `libxcselect` when no sysroot is given. Without it, a compilation whose SDK
/// discovery failed builds against an SDK the catalog never records.
pub(super) const NO_XCSELECT: &str = "--no-xcselect";

/// The SDK one generation run discovered, and which compilers accept
/// [`NO_XCSELECT`] (LLVM 23 and newer; older clang and Apple clang reject it).
#[derive(Default)]
pub(super) struct MacosSdk {
    discovered: OnceLock<Result<String, String>>,
    no_xcselect: Mutex<BTreeMap<PathBuf, bool>>,
}

impl MacosSdk {
    pub(super) fn infer(
        &self,
        arguments: &[String],
        compiler_version: &str,
        environment: &BTreeMap<String, String>,
        discover: impl FnOnce() -> Result<String, String>,
    ) -> Result<Option<String>, String> {
        if environment.contains_key("SDKROOT")
            || [
                "IPHONEOS_DEPLOYMENT_TARGET",
                "TVOS_DEPLOYMENT_TARGET",
                "WATCHOS_DEPLOYMENT_TARGET",
                "VISIONOS_DEPLOYMENT_TARGET",
            ]
            .iter()
            .any(|name| environment.contains_key(*name))
        {
            return Ok(None);
        }
        let mut target = compiler_version
            .lines()
            .find_map(|line| line.strip_prefix("Target: "))
            .unwrap_or_default();
        let mut args = arguments.iter();
        while let Some(arg) = args.next() {
            if arg.starts_with("-isysroot") || arg == "--sysroot" || arg.starts_with("--sysroot=") {
                return Ok(None);
            }
            if [
                "-miphone",
                "-mios",
                "-mtvos",
                "-mwatchos",
                "-mvisionos",
                "-mxros",
            ]
            .iter()
            .any(|prefix| arg.starts_with(prefix))
                || arg
                    .strip_prefix("-mtargetos=")
                    .is_some_and(|os| !os.starts_with("macos"))
            {
                return Ok(None);
            }
            if arg == "-target" || arg == "--target" {
                target = args.next().map(String::as_str).unwrap_or_default();
            } else if let Some(value) = arg
                .strip_prefix("--target=")
                .or_else(|| arg.strip_prefix("-target="))
            {
                target = value;
            } else {
                // A flag-shaped include path or macro value is not an option.
                let arity = arg_exact_match_map()
                    .get(arg.as_str())
                    .or_else(|| arg_patterns().first_match(arg))
                    .map_or(0, |info| info.arity);
                for _ in 0..arity {
                    args.next();
                }
            }
        }
        let macos = target
            .split('-')
            .nth(2)
            .is_some_and(|os| os.starts_with("darwin") || os.starts_with("macos"));
        if !macos {
            return Ok(None);
        }
        self.discovered.get_or_init(discover).clone().map(Some)
    }

    /// Whether `compiler` accepts [`NO_XCSELECT`], asked once per compiler.
    pub(super) fn accepts_no_xcselect(
        &self,
        compiler: &Path,
        accepts: impl FnOnce() -> bool,
    ) -> bool {
        let mut known = self
            .no_xcselect
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *known.entry(compiler.to_path_buf()).or_insert_with(accepts)
    }
}

/// Whether `compiler` accepts [`NO_XCSELECT`]: an unknown driver flag fails
/// even `--version`.
pub(super) fn probe_no_xcselect(
    compiler: &Path,
    directory: &Path,
    environment: &BTreeMap<String, String>,
) -> bool {
    rllvm_core::utils::execute_llvm_tool_in_for_output(
        compiler,
        &[NO_XCSELECT, "--version"],
        directory,
        environment,
    )
    .is_ok_and(|output| output.status.success())
}

pub(super) fn discover(environment: &BTreeMap<String, String>) -> Result<String, String> {
    let output = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .env_clear()
        .envs(environment)
        .output()
        .map_err(|error| format!("cannot run xcrun: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "xcrun exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sdk = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let sdk = sdk.trim();
    if !Path::new(sdk).is_absolute() || !Path::new(sdk).is_dir() {
        return Err("xcrun did not return an existing absolute SDK directory".into());
    }
    Ok(sdk.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const CLANG: &str = "Homebrew clang\nTarget: arm64-apple-darwin25.0.0";

    #[test]
    fn no_xcselect_support_is_probed_once_per_compiler() {
        let sdk = MacosSdk::default();
        let probes = Cell::new(0);
        let probe = |answer| {
            probes.set(probes.get() + 1);
            answer
        };
        for _ in 0..3 {
            assert!(sdk.accepts_no_xcselect(Path::new("/llvm23/clang"), || probe(true)));
        }
        assert!(!sdk.accepts_no_xcselect(Path::new("/usr/bin/clang"), || probe(false)));
        assert_eq!(probes.get(), 2);
    }

    #[test]
    fn sdk_inference_respects_explicit_settings_and_cross_targets() {
        let sdk = MacosSdk::default();
        for args in [
            vec!["-isysroot", "sdk"],
            vec!["-isysrootsdk"],
            vec!["--sysroot", "sdk"],
            vec!["--sysroot=sdk"],
            vec!["--target=wasm32-unknown-unknown"],
            vec!["-target", "aarch64-unknown-linux-gnu"],
            vec!["--target", "arm64-apple-ios17.0"],
            vec!["-miphoneos-version-min=17.0"],
            vec!["-mtargetos=ios17.0"],
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert_eq!(
                sdk.infer(&args, CLANG, &BTreeMap::new(), || panic!(
                    "unwanted SDK discovery"
                ))
                .unwrap(),
                None
            );
        }
        for name in ["SDKROOT", "IPHONEOS_DEPLOYMENT_TARGET"] {
            let env = BTreeMap::from([(name.into(), "explicit".into())]);
            assert_eq!(
                sdk.infer(&[], CLANG, &env, || panic!("explicit SDK overwritten"))
                    .unwrap(),
                None
            );
        }
        for version in ["Target: x86_64-unknown-linux-gnu", "unknown target"] {
            assert_eq!(
                sdk.infer(&[], version, &BTreeMap::new(), || panic!(
                    "non-macOS compiler"
                ))
                .unwrap(),
                None
            );
        }
    }

    #[test]
    fn sdk_discovery_is_cached_and_respects_option_operands() {
        let sdk = MacosSdk::default();
        let calls = Cell::new(0);
        for args in [
            vec![],
            vec!["-I", "--target=wasm32-unknown-unknown", "-D", "-isysroot"],
            vec![
                "--target=wasm32-unknown-unknown",
                "-target",
                "x86_64-apple-macosx13",
            ],
        ] {
            let args: Vec<_> = args.into_iter().map(String::from).collect();
            assert_eq!(
                sdk.infer(&args, CLANG, &BTreeMap::new(), || {
                    calls.set(calls.get() + 1);
                    Ok("selected-sdk".into())
                })
                .unwrap()
                .as_deref(),
                Some("selected-sdk")
            );
        }
        assert_eq!(calls.get(), 1);
        let failed = MacosSdk::default();
        for _ in 0..2 {
            assert!(
                failed
                    .infer(&[], CLANG, &BTreeMap::new(), || {
                        calls.set(calls.get() + 1);
                        Err("no SDK installed".into())
                    })
                    .is_err()
            );
        }
        assert_eq!(calls.get(), 2, "failed discovery is cached too");
    }
}
