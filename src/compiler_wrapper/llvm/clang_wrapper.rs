//! Clang compiler wrapper

use std::path::{Path, PathBuf};

use crate::{
    arg_parser::CompilerArgsInfo, compiler_wrapper::*, config::RLLVM_CONFIG, error::Error,
};

#[derive(Debug)]
pub struct ClangWrapper {
    name: String,
    wrapped_compiler: PathBuf,
    compiler_kind: CompilerKind,
    is_silent: bool,

    is_parse_args_called: bool,

    args: CompilerArgsInfo,
}

impl ClangWrapper {
    pub fn new(name: &str, compiler_kind: CompilerKind) -> Self {
        // Obtain the compiler path from the configuration
        let compiler_path = match compiler_kind {
            CompilerKind::Clang => RLLVM_CONFIG.clang_filepath(),
            CompilerKind::ClangXX => RLLVM_CONFIG.clangxx_filepath(),
        };

        Self {
            name: name.to_string(),
            wrapped_compiler: compiler_path.clone(),
            compiler_kind,
            is_silent: false,
            is_parse_args_called: false,
            args: CompilerArgsInfo::default(),
        }
    }
}

impl CompilerWrapper for ClangWrapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn wrapped_compiler(&self) -> &Path {
        &self.wrapped_compiler
    }

    fn compiler_kind(&self) -> &CompilerKind {
        &self.compiler_kind
    }

    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>,
    {
        // Empty argument list is not allowed
        if args.is_empty() {
            return Err(Error::InvalidArguments(
                "The give argument list cannot be empty".to_string(),
            ));
        }

        if self.is_parse_args_called {
            return Err(Error::Unknown(
                "parse_args() cannot be called twice on the same instance".to_string(),
            ));
        }
        self.is_parse_args_called = true;

        self.args.parse_args(args)?;

        Ok(self)
    }

    fn args(&self) -> &CompilerArgsInfo {
        &self.args
    }

    fn silence(&mut self, value: bool) -> &'_ mut Self {
        self.is_silent = value;
        self
    }

    fn is_silent(&self) -> bool {
        self.is_silent
    }
}
