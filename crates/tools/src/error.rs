//! Error types for the rllvm crate.
//!
//! Provides a unified [`Error`] enum covering I/O failures, object file
//! manipulation errors, configuration issues, and more.

use std::{
    path::{Path, PathBuf},
    str::Utf8Error,
    string::FromUtf8Error,
};

/// The error type for rllvm operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid arguments
    #[error("Invalid arguments: {0}")]
    InvalidArguments(String),
    /// Io error occurred, with no file to attribute it to.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// An I/O failure on a specific file.
    ///
    /// The path belongs in the error rather than only in a log line: several
    /// of these fire before a `tracing` subscriber is installed, and a user
    /// who mistyped a filename needs the filename, not an errno.
    #[error("{path}: {source}")]
    File {
        /// The file the operation was attempted on.
        path: PathBuf,
        /// What the operating system reported.
        #[source]
        source: std::io::Error,
    },
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

impl Error {
    /// An I/O failure on `path`, so the message can name the file.
    pub fn file(path: impl AsRef<Path>, source: std::io::Error) -> Error {
        Error::File {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

/// Print `result`'s error for a human and turn it into an exit code.
///
/// Binaries call this instead of returning `Result` from `main`. A `main`
/// that returns `Err` is rendered by the runtime with `Debug`, which is how
/// `Error: Io(Os { code: 2, kind: NotFound, ... })` used to reach someone who
/// had merely mistyped a filename. Diagnostics go to stderr because these
/// wrappers stand in for a compiler, and build systems read compiler stdout.
pub fn report(result: Result<(), Error>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
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
