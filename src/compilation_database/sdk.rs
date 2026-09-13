//! Optional macOS SDK discovery for imported compilations.

use std::{collections::BTreeMap, path::Path, process::Command, sync::OnceLock};

use crate::constants::{arg_exact_match_map, arg_patterns};

#[derive(Default)]
pub(super) struct MacosSdk(OnceLock<Result<String, String>>);

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
        self.0.get_or_init(discover).clone().map(Some)
    }
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
