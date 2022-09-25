//! Genera interfaces for the compiler wrapper

/// Compiler wrapper error Type
#[derive(Debug)]
pub enum Error {
    /// Invalid arguments are passed to the compiler wrapper
    InvalidArguments(String),
    /// Io error occurred
    Io(std::io::Error),
    /// Something else happened
    Unknown(String),
}

/// A general interface that wraps different compilers
pub trait CompilerWrapper {
    /// Set the wrapper arguments parsing a command line set of arguments
    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>;
}
