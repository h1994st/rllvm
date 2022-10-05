//! Common interfaces/functions shared among other modules

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

pub(self) mod constants;

mod arg_parser;
pub use arg_parser::*;

mod compiler_wrapper;
pub use compiler_wrapper::*;

pub mod utils;
