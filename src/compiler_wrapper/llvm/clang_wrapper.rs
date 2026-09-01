//! Clang compiler wrapper

use std::path::{Path, PathBuf};

use crate::{
    arg_parser::CompilerArgsInfo, compiler_wrapper::*, config::try_rllvm_config, error::Error,
};

/// Clang/Clang++ compiler wrapper that generates LLVM bitcode alongside normal compilation.
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
    pub fn new(name: &str, compiler_kind: CompilerKind) -> Result<Self, Error> {
        // Obtain the compiler path from the configuration
        let config = try_rllvm_config()?;
        let compiler_path = match compiler_kind {
            CompilerKind::Clang => config.clang_filepath(),
            CompilerKind::ClangXX => config.clangxx_filepath(),
        };

        Ok(Self {
            name: name.to_string(),
            wrapped_compiler: compiler_path.clone(),
            compiler_kind,
            is_silent: false,
            is_parse_args_called: false,
            args: CompilerArgsInfo::default(),
        })
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

/// Builder for the [`ClangWrapper`]
#[derive(Debug)]
pub struct ClangWrapperBuilder {
    /// Name of the wrapper
    name: String,
    /// Path to the wrapped compiler (optional)
    wrapped_compiler: Option<PathBuf>,
    /// Compiler kind
    compiler_kind: CompilerKind,
    /// Silence the compiler wrapper output (optional)
    is_silent: Option<bool>,
}

impl Default for ClangWrapperBuilder {
    fn default() -> Self {
        Self {
            name: String::new(),
            wrapped_compiler: None,
            compiler_kind: CompilerKind::Clang,
            is_silent: None,
        }
    }
}

impl ClangWrapperBuilder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CompilerWrapperBuilder for ClangWrapperBuilder {
    type OutputType = ClangWrapper;

    fn build(&self) -> Result<Self::OutputType, Error> {
        // Obtain the compiler path from the configuration, if not provided.
        // The configuration is only consulted when no path was supplied, so a
        // caller that provides one can run without a usable config file.
        let compiler_path = match self.wrapped_compiler.as_ref() {
            Some(compiler_path) => compiler_path.clone(),
            None => {
                let config = try_rllvm_config()?;
                match self.compiler_kind {
                    CompilerKind::Clang => config.clang_filepath().clone(),
                    CompilerKind::ClangXX => config.clangxx_filepath().clone(),
                }
            }
        };

        Ok(ClangWrapper {
            name: self.name.clone(),
            wrapped_compiler: compiler_path,
            compiler_kind: self.compiler_kind,
            is_silent: self.is_silent.unwrap_or(false),
            is_parse_args_called: false,
            args: CompilerArgsInfo::default(),
        })
    }

    fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    fn compiler_kind(mut self, compiler_kind: CompilerKind) -> Self {
        self.compiler_kind = compiler_kind;
        self
    }

    fn wrapped_compiler<P>(mut self, wrapped_compiler: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.wrapped_compiler = Some(wrapped_compiler.as_ref().to_path_buf());
        self
    }

    fn silence(mut self, value: bool) -> Self {
        self.is_silent = Some(value);
        self
    }
}
