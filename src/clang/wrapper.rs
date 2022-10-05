//! Clang compiler wrapper

use crate::common::*;

#[derive(Debug, Default)]
pub struct ClangWrapper {
    name: String,
    wrapped_cc: String,
    wrapped_cxx: String,
    is_silent: bool,

    parse_args_called: bool,

    args: CompilerArgsInfo,
}

impl CompilerWrapper for ClangWrapper {
    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>,
    {
        // Empty argument list is not allowed
        if args.len() <= 1 {
            return Err(Error::InvalidArguments(
                "The number of arguments cannot be empty".to_string(),
            ));
        }

        if self.parse_args_called {
            return Err(Error::Unknown(
                "parse_args() cannot be called twice on the same instance".to_string(),
            ));
        }
        self.parse_args_called = true;

        self.name = args[0].as_ref().to_string();

        self.args
            .parse_args(args)
            .expect("Failed to parse arguments!");

        todo!()
    }

    fn args(&self) -> &CompilerArgsInfo {
        &self.args
    }

    fn args_mut(&mut self) -> &mut CompilerArgsInfo {
        &mut self.args
    }

    fn command(&mut self) -> Result<Vec<String>, Error> {
        todo!()
    }

    fn silence(&mut self, value: bool) -> &'_ mut Self {
        self.is_silent = value;
        self
    }

    fn is_silent(&self) -> bool {
        self.is_silent
    }
}
