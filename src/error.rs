//! rllvm error Type

#[derive(Debug)]
pub enum Error {
    /// Invalid arguments
    InvalidArguments(String),
    /// Io error occurred
    Io(std::io::Error),
    /// Execution failure
    ExecutionFailure(String),
    /// Something else happened
    Unknown(String),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
