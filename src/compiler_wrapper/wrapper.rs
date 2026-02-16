//! Genera interfaces for the compiler wrapper

use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    arg_parser::{CompileMode, CompilerArgsInfo},
    config::rllvm_config,
    error::Error,
    utils::{embed_bitcode_filepath_to_object_file, execute_command_for_status},
};

/// Compiler type
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CompilerKind {
    /// Clang
    #[default]
    Clang,
    /// Clang++
    ClangXX,
}

/// A general interface that wraps different compilers
pub trait CompilerWrapper {
    /// Obtain the name of the wrapper
    fn name(&self) -> &str;

    /// Obtain the path to the wrapped compiler
    fn wrapped_compiler(&self) -> &Path;

    /// Obtain the compiler kind
    fn compiler_kind(&self) -> &CompilerKind;

    /// Set the wrapper arguments parsing a command line set of arguments
    #[must_use]
    fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>;

    /// Obtain the argument information
    fn args(&self) -> &CompilerArgsInfo;

    /// Command to run the compiler
    fn command(&self) -> Result<Vec<String>, Error> {
        let args_info = self.args();
        let compiler_filepath = self.wrapped_compiler();
        let mut args = vec![String::from(compiler_filepath.to_string_lossy())];

        // Append LTO LDFLAGS
        if args_info.input_files().is_empty() && !args_info.link_args().is_empty() {
            // Linking
            if args_info.is_lto() {
                // Add LTO LDFLAGS
                if let Some(lto_ldflags) = rllvm_config().lto_ldflags() {
                    args.extend(lto_ldflags.iter().cloned());
                }
            }
        }

        // Append given arguments
        args.extend(args_info.input_args().iter().cloned());

        // Remove forbidden flags
        if !args_info.forbidden_flags().is_empty() {
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
        if let Some(code) = self.build_target()? {
            if code != 0 {
                return Ok(Some(code));
            }
        }
        if self.args().is_bitcode_generation_skipped() {
            return Ok(Some(0));
        }

        self.generate_bitcode_files_and_embed_filepaths()
    }

    fn execute_command<S>(&self, args: &[S], mode: CompileMode) -> Result<Option<i32>, Error>
    where
        S: AsRef<OsStr> + std::fmt::Debug,
    {
        if !self.is_silent() {
            log::debug!("[{:?}] args={:?}", mode, args);
        }
        if args.is_empty() {
            return Err(Error::InvalidArguments(
                "The number of arguments cannot be 0".into(),
            ));
        }
        let status = execute_command_for_status(args[0].as_ref(), &args[1..])?;
        if !self.is_silent() {
            log::debug!("[{:?}] exit_status={}", mode, status);
        }

        if !status.success() {
            return Err(Error::ExecutionFailure(format!(
                "Failed to execute the command: args={:?}, exit_status={}",
                args, status
            )));
        }

        Ok(status.code())
    }

    /// Execute the given command and build the target
    fn build_target(&self) -> Result<Option<i32>, Error> {
        let args = self.command()?;
        let mode = self.args().mode();

        self.execute_command(&args, mode)
    }

    /// Generate bitcode files for all input files
    fn generate_bitcode_files_and_embed_filepaths(&self) -> Result<Option<i32>, Error> {
        let is_compile_only = self.args().is_compile_only();
        let artifact_filepaths = self.args().artifact_filepaths()?;
        let mut object_filepaths = vec![];
        for (src_filepath, object_filepath, bitcode_filepath) in artifact_filepaths {
            if !is_compile_only {
                // We need to explicitly build the intermediate object file
                self.build_object_file(&src_filepath, &object_filepath)?;

                // Collect all intermediate object files
                object_filepaths.push(object_filepath.clone());
            }

            let src_bitcode_filepath = if src_filepath.extension().map_or(false, |x| x == "bc") {
                // The source file is a bitcode; therefore, we do not need to
                // generate the bitcode and directly use the source file
                src_filepath
            } else {
                // Generate the bitcode
                if let Some(code) = self.generate_bitcode_file(&src_filepath, &bitcode_filepath)? {
                    if code != 0 {
                        return Ok(Some(code));
                    }
                }
                bitcode_filepath
            };

            // Embed the path of the bitcode to the corresponding object file
            embed_bitcode_filepath_to_object_file(&src_bitcode_filepath, &object_filepath, None)?;
        }

        let output_filepath = PathBuf::from(self.args().output_filename()).canonicalize()?;
        self.link_object_files(&object_filepaths, output_filepath)
    }

    /// Generate bitcode file for one input file
    fn generate_bitcode_file<P>(
        &self,
        src_filepath: P,
        bitcode_filepath: P,
    ) -> Result<Option<i32>, Error>
    where
        P: AsRef<Path>,
    {
        let src_filepath = src_filepath.as_ref();
        let bitcode_filepath = bitcode_filepath.as_ref();
        let compiler_filepath = self.wrapped_compiler();

        let mut args = vec![String::from(compiler_filepath.to_string_lossy())];
        args.extend(self.args().compile_args().iter().cloned());
        // Add bitcode generation flags
        if let Some(bitcode_generation_flags) = rllvm_config().bitcode_generation_flags() {
            args.extend(bitcode_generation_flags.iter().cloned());
        }
        args.extend_from_slice(&[
            "-emit-llvm".to_string(),
            "-c".to_string(),
            "-o".to_string(),
            String::from(bitcode_filepath.to_string_lossy()),
            String::from(src_filepath.to_string_lossy()),
        ]);

        let mode = CompileMode::BitcodeGeneration;

        self.execute_command(&args, mode)
    }

    /// Execute the command and build the object file
    fn build_object_file<P>(
        &self,
        src_filepath: P,
        object_filepath: P,
    ) -> Result<Option<i32>, Error>
    where
        P: AsRef<Path>,
    {
        let src_filepath = src_filepath.as_ref();
        let object_filepath = object_filepath.as_ref();
        let wrapped_compiler = self.wrapped_compiler();

        let mut args = vec![String::from(wrapped_compiler.to_string_lossy())];
        args.extend(self.args().compile_args().iter().cloned());
        args.extend_from_slice(&[
            "-c".to_string(),
            "-o".to_string(),
            String::from(object_filepath.to_string_lossy()),
            String::from(src_filepath.to_string_lossy()),
        ]);

        let mode = CompileMode::Compiling;

        self.execute_command(&args, mode)
    }

    fn link_object_files<P>(
        &self,
        object_filepaths: &[P],
        output_filepath: P,
    ) -> Result<Option<i32>, Error>
    where
        P: AsRef<Path>,
    {
        let output_filepath = output_filepath.as_ref();
        let wrapped_compiler = self.wrapped_compiler();

        let mut args = vec![String::from(wrapped_compiler.to_string_lossy())];
        if self.args().is_lto() {
            // Add LTO LDFLAGS
            if let Some(lto_ldflags) = rllvm_config().lto_ldflags() {
                args.extend(lto_ldflags.iter().cloned());
            }
        }
        // Link arguments
        args.extend(self.args().link_args().iter().cloned());
        // Output
        args.extend_from_slice(&[
            "-o".to_string(),
            String::from(output_filepath.to_string_lossy()),
        ]);
        // Input object files
        args.extend(
            object_filepaths
                .iter()
                .map(|x| String::from(x.as_ref().to_string_lossy())),
        );

        // Mode
        let mode = CompileMode::Linking;

        self.execute_command(&args, mode)
    }
}

/// A general interface for the compiler wrapper builder
pub trait CompilerWrapperBuilder {
    type OutputType;

    /// Build the compiler wrapper
    fn build(&self) -> Self::OutputType;

    /// Set the compiler name
    #[must_use]
    fn name(self, name: &str) -> Self;

    /// Set the compiler kind
    #[must_use]
    fn compiler_kind(self, compiler_kind: CompilerKind) -> Self;

    /// Set the wrapped compiler path
    fn wrapped_compiler<P>(self, wrapped_compiler: P) -> Self
    where
        P: AsRef<Path>;

    /// Set the silence flag
    fn silence(self, value: bool) -> Self;
}
