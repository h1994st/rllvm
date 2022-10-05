//! Genera interfaces for the compiler wrapper

use std::process::Command;

use crate::common::{CompilerArgsInfo, Error};

/// A general interface that wraps different compilers
pub trait CompilerWrapper {
    /// Set the wrapper arguments parsing a command line set of arguments
    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>;

    /// Obtain the argument information
    fn args(&self) -> &CompilerArgsInfo;

    /// Obtain the argument information (mutable)
    fn args_mut(&mut self) -> &mut CompilerArgsInfo;

    /// Command to run the compiler
    fn command(&mut self) -> Result<Vec<String>, Error>;

    /// Silences the compiler wrapper output
    fn silence(&mut self, value: bool) -> &'_ mut Self;

    /// Returns `true` if `silence` was called with `true`
    fn is_silent(&self) -> bool;

    /// Run the compiler
    fn run(&mut self) -> Result<Option<i32>, Error> {
        let args = self.command()?;

        if !self.is_silent() {
            dbg!(&args);
        }
        if args.is_empty() {
            return Err(Error::InvalidArguments(
                "The number of arguments cannot be 0".into(),
            ));
        }
        let status = match Command::new(&args[0]).args(&args[1..]).status() {
            Ok(s) => s,
            Err(e) => return Err(Error::Io(e)),
        };
        if !self.is_silent() {
            dbg!(status);
        }
        Ok(status.code())
    }
}
