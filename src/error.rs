//! rllvm error Type

use std::{str::Utf8Error, string::FromUtf8Error};

#[derive(Debug)]
pub enum Error {
    /// Invalid arguments
    InvalidArguments(String),
    /// Io error occurred
    Io(std::io::Error),
    /// Command execution failure
    ExecutionFailure(String),
    /// Object file error
    ObjectReadError(object::read::Error),
    ObjectWriteError(object::write::Error),
    /// String error
    StringError(String),
    /// Logger error
    LoggerError(String),
    /// Missing file
    MissingFile(String),
    /// Something else happened
    Unknown(String),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<object::read::Error> for Error {
    fn from(value: object::read::Error) -> Self {
        Self::ObjectReadError(value)
    }
}

impl From<object::write::Error> for Error {
    fn from(value: object::write::Error) -> Self {
        Self::ObjectWriteError(value)
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
