//! Genera interfaces for the compiler wrapper

use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    vec,
};

use crate::{
    arg_parser::{CompileMode, CompilerArgsInfo},
    error::Error,
    utils::embed_bitcode_filepath_to_object_file,
};

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
        let mut args = vec![String::from(program_filepath.to_string_lossy())];

        // Append LTO LDFLAGS
        if args_info.input_files().is_empty() && args_info.link_args().len() > 0 {
            // Linking
            if args_info.is_lto() {
                // TODO: add LTO LDFLAGS
                todo!();
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
    fn run(&mut self) -> Result<(), Error> {
        if self.args().is_bitcode_generation_skipped() {
            return self.build_target();
        }

        todo!()
    }

    fn execute_command<S>(&self, args: &[S], mode: CompileMode) -> Result<(), Error>
    where
        S: AsRef<OsStr> + std::fmt::Debug,
    {
        if !self.is_silent() {
            log::debug!("[{:?}] Arguments: {:?}", mode, args);
        }
        if args.is_empty() {
            return Err(Error::InvalidArguments(
                "The number of arguments cannot be 0".into(),
            ));
        }
        let status = Command::new(args[0].as_ref()).args(&args[1..]).status()?;
        if !self.is_silent() {
            log::debug!("[{:?}] Exit status: {}", mode, status);
        }

        if !status.success() {
            return Err(Error::ExecutionFailure(format!(
                "Failed to execute the command: {}",
                status
            )));
        }

        Ok(())
    }

    /// Execute the given command and build the target
    fn build_target(&self) -> Result<(), Error> {
        let args = self.command()?;
        let mode = self.args().mode();

        self.execute_command(&args, mode)
    }

    /// Generate bitcodes for all input files
    fn generate_bitcodes_and_embed_filepaths(&self) -> Result<(), Error> {
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
                self.generate_bitcode(&src_filepath, &bitcode_filepath)?;
                bitcode_filepath
            };

            // Embed the path of the bitcode to the corresponding object file
            embed_bitcode_filepath_to_object_file(&src_bitcode_filepath, &object_filepath, None)?;
        }

        let output_filepath = PathBuf::from(self.args().output_filename()).canonicalize()?;
        self.link_object_files(&object_filepaths, output_filepath)?;

        Ok(())
    }

    /// Generate bitcode for one input file
    fn generate_bitcode<P>(&self, src_filepath: P, bitcode_filepath: P) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        let src_filepath = src_filepath.as_ref();
        let bitcode_filepath = bitcode_filepath.as_ref();
        let program_filepath = self.program_filepath();

        let mut args = vec![String::from(program_filepath.to_string_lossy())];
        args.extend(self.args().compile_args().iter().cloned());
        // TODO: add other bitcode generation flags
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
    fn build_object_file<P>(&self, src_filepath: P, object_filepath: P) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        let src_filepath = src_filepath.as_ref();
        let object_filepath = object_filepath.as_ref();
        let program_filepath = self.program_filepath();

        let mut args = vec![String::from(program_filepath.to_string_lossy())];
        args.extend(self.args().compile_args().iter().cloned());
        // TODO: add other bitcode generation flags
        args.extend_from_slice(&[
            "-c".to_string(),
            "-o".to_string(),
            String::from(object_filepath.to_string_lossy()),
            String::from(src_filepath.to_string_lossy()),
        ]);

        let mode = CompileMode::Compiling;

        self.execute_command(&args, mode)
    }

    fn link_object_files<P>(&self, object_filepaths: &[P], output_filepath: P) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        let output_filepath = output_filepath.as_ref();
        let program_filepath = self.program_filepath();

        let mut args = vec![String::from(program_filepath.to_string_lossy())];
        if self.args().is_lto() {
            // TODO: add LTO LDFLAGS
            todo!()
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
