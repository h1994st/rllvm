//! Genera interfaces for the compiler wrapper

use std::{collections::HashSet, path::Path, process::Command};

use crate::{arg_parser::CompilerArgsInfo, error::Error};

/// A general interface that wraps different compilers
pub trait CompilerWrapper {
    /// Obtain the path to the wrapped compiler
    fn program_filepath(&self) -> &Path;

    /// Set the wrapper arguments parsing a command line set of arguments
    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>;

    /// Obtain the argument information
    fn args(&self) -> &CompilerArgsInfo;

    /// Obtain the argument information (mutable)
    fn args_mut(&mut self) -> &mut CompilerArgsInfo;

    /// Command to run the compiler
    fn command(&self) -> Result<Vec<String>, Error> {
        let args_info = self.args();
        let program_filepath = self.program_filepath();
        let mut args = vec![program_filepath.to_string_lossy().into()];

        // Append LTO LDFLAGS
        if args_info.input_files().is_empty() && args_info.link_args().len() > 0 {
            // Linking
            if args_info.is_lto() {
                // TODO: add LTO LDFLAGS
            }
        }

        // Append given arguments
        args.extend(args_info.input_args().iter().cloned());

        // Remove forbidden flags
        if args_info.forbidden_flags().len() > 0 {
            let forbidden_flags_set: HashSet<String> =
                HashSet::from_iter(args_info.forbidden_flags().iter().cloned());
            args = args
                .into_iter()
                .filter(|x| !forbidden_flags_set.contains(x))
                .collect();
        }

        Ok(args)
    }

    /// Silences the compiler wrapper output
    fn silence(&mut self, value: bool) -> &'_ mut Self;

    /// Returns `true` if `silence` was called with `true`
    fn is_silent(&self) -> bool;

    /// Run the compiler
    fn run(&mut self) -> Result<Option<i32>, Error> {
        if self.args().is_bitcode_generation_skipped() {
            return self.build_target();
        }

        todo!()
    }

    /// Execute the given command and build the target
    fn build_target(&self) -> Result<Option<i32>, Error> {
        let args = self.command()?;
        let mode = self.args().mode();

        if !self.is_silent() {
            log::debug!("[{:?}] Arguments: {:?}", mode, &args);
        }
        if args.is_empty() {
            return Err(Error::InvalidArguments(
                "The number of arguments cannot be 0".into(),
            ));
        }
        let status = Command::new(&args[0]).args(&args[1..]).status()?;
        if !self.is_silent() {
            log::debug!("[{:?}] Exit status: {}", mode, status);
        }
        Ok(status.code())
    }

    /// Generate bitcodes for all input files
    fn generate_bitcodes(&self) {
        todo!()
    }
}
