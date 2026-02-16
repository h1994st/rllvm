//! rllvm error Type

use std::{str::Utf8Error, string::FromUtf8Error};

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
    /// Logger error
    #[error("Logger error: {0}")]
    LoggerError(String),
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
