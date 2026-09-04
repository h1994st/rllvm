//! Error types for the rllvm crate.
//!
//! Provides a unified [`Error`] enum covering I/O failures, object file
//! manipulation errors, configuration issues, and more.

use std::{str::Utf8Error, string::FromUtf8Error};

/// The error type for rllvm operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid arguments
    #[error("Invalid arguments: {0}")]
    InvalidArguments(String),
    /// Io error occurred
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Command execution failure
    #[error("Execution failure: {0}")]
    ExecutionFailure(String),
    /// Object file read error
    #[error("Object read error: {0}")]
    ObjectReadError(#[from] object::read::Error),
    /// Object file write error
    #[error("Object write error: {0}")]
    ObjectWriteError(#[from] object::write::Error),
    /// String error
    #[error("String error: {0}")]
    StringError(String),
    /// Unsupported binary format
    #[error("Unsupported binary format: {0}")]
    UnsupportedBinaryFormat(String),
    /// Missing file
    #[error("Missing file: {0}")]
    MissingFile(String),
    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),
    /// Something else happened
    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl From<Utf8Error> for Error {
    fn from(value: Utf8Error) -> Self {
        Self::StringError(format!("{}", value))
    }
}

impl From<FromUtf8Error> for Error {
    fn from(value: FromUtf8Error) -> Self {
        Self::StringError(format!("{}", value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_errors_convert_to_string_errors() {
        // Both From impls exist so `?` works on UTF-8 conversion failures.
        // Built at runtime: clippy rejects a literal it can prove is invalid.
        let invalid: Vec<u8> = vec![0x66, 0x6f, 0x80];
        let utf8_err = std::str::from_utf8(&invalid).unwrap_err();
        let err: Error = utf8_err.into();
        assert!(matches!(err, Error::StringError(_)));
        assert!(err.to_string().contains("String error"));

        let from_utf8_err = String::from_utf8(invalid.clone()).unwrap_err();
        let err: Error = from_utf8_err.into();
        assert!(matches!(err, Error::StringError(_)));
    }

    #[test]
    fn every_variant_renders_a_message() {
        let cases = [
            Error::InvalidArguments("a".into()),
            Error::ExecutionFailure("b".into()),
            Error::StringError("c".into()),
            Error::UnsupportedBinaryFormat("d".into()),
            Error::MissingFile("e".into()),
            Error::ConfigError("f".into()),
            Error::Unknown("g".into()),
            Error::Io(std::io::Error::other("h")),
        ];
        for err in cases {
            assert!(!err.to_string().is_empty(), "empty Display for {err:?}");
        }
    }
}
