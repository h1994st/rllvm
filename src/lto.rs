//! Link-time optimization support.
//!
//! With `-flto` the compiler writes a bitcode module where an object file
//! belongs, so there is no section header to record a bitcode path in. Two
//! mechanisms answer that, and [`LtoMode`] chooses between them.

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// How the wrappers handle a build that enables link-time optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LtoMode {
    /// Compile a marker module naming each translation unit's bitcode and
    /// merge it into the LTO object. Works on every linker and both LTO
    /// flavours, and costs one extra compile per translation unit.
    #[default]
    Marker,
    /// Ask the linker to keep the module its own LTO pipeline merged. Full
    /// LTO only, and the link has to go through the wrapper.
    SaveTemps,
    /// Generate nothing, and say so. The behaviour before #96.
    Skip,
}

impl std::fmt::Display for LtoMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LtoMode::Marker => write!(f, "marker"),
            LtoMode::SaveTemps => write!(f, "save-temps"),
            LtoMode::Skip => write!(f, "skip"),
        }
    }
}

impl std::str::FromStr for LtoMode {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "marker" => Ok(Self::Marker),
            "save-temps" => Ok(Self::SaveTemps),
            "skip" => Ok(Self::Skip),
            other => Err(Error::ConfigError(format!(
                "Unknown lto_mode {other:?}; expected one of: marker, save-temps, skip"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lto_mode_defaults_to_marker() {
        assert_eq!(LtoMode::default(), LtoMode::Marker);
    }

    #[test]
    fn lto_mode_round_trips_through_its_documented_spelling() {
        for mode in [LtoMode::Marker, LtoMode::SaveTemps, LtoMode::Skip] {
            let spelled = mode.to_string();
            assert_eq!(spelled.parse::<LtoMode>().unwrap(), mode, "{spelled}");
        }
    }

    #[test]
    fn unknown_lto_mode_is_an_error_not_a_fallback() {
        // A typo must stop the build, not silently disable extraction.
        let err = "marker ".parse::<LtoMode>().unwrap_err();
        assert!(matches!(err, Error::ConfigError(_)));
        assert!(err.to_string().contains("save-temps"), "{err}");
    }
}
