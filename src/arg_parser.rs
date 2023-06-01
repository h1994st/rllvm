//! Command-line argument parser

use std::path::PathBuf;

use lazy_static::lazy_static;
use regex::Regex;

use crate::{
    constants::{ARG_EXACT_MATCH_MAP, ARG_PATTERNS},
    error::Error,
    utils::*,
};

/// Compile mode
#[derive(Debug)]
pub enum CompileMode {
    /// Compiling mode
    Compiling,
    /// Linking mode
    Linking,
    /// Link Time Optimization mode
    LTO,
    /// Bitcode Generation mode
    BitcodeGeneration,
}

/// Compiler argument information
#[derive(Debug, Default)]
pub struct CompilerArgsInfo {
    input_args: Vec<String>,
    input_files: Vec<String>,
    object_files: Vec<String>,
    output_filename: String,
    compile_args: Vec<String>,
    link_args: Vec<String>,
    forbidden_flags: Vec<String>,
    is_verbose: bool,
    is_dependency_only: bool,
    is_preprocess_only: bool,
    is_assemble_only: bool,
    is_assembly: bool,
    is_compile_only: bool,
    is_emit_llvm: bool,
    is_lto: bool,
    is_print_only: bool,
}

pub type CallbackFn<S> = for<'a> fn(&'a mut CompilerArgsInfo, S, &[S]) -> &'a mut CompilerArgsInfo;
pub type Callback<S> = Box<CallbackFn<S>>;

pub struct ArgInfo<S>
where
    S: AsRef<str>,
{
    pub arity: usize,
    pub handler: CallbackFn<S>,
}

impl<S> ArgInfo<S>
where
    S: AsRef<str>,
{
    pub fn new(arity: usize, handler: CallbackFn<S>) -> Self {
        Self { arity, handler }
    }
}

pub struct ArgPatternInfo<S>
where
    S: AsRef<str>,
{
    pub pattern: Regex,
    pub arg_info: ArgInfo<S>,
}

impl<S> ArgPatternInfo<S>
where
    S: AsRef<str>,
{
    pub fn new(pattern: &str, arity: usize, handler: CallbackFn<S>) -> Self {
        let pattern = Regex::new(pattern).unwrap();
        let arg_info = ArgInfo::new(arity, handler);
        Self { pattern, arg_info }
    }
}

impl CompilerArgsInfo {
    pub fn input_file<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.input_files.push(flag.as_ref().to_string());

        // Assembly files
        lazy_static! {
            static ref RE: Regex = Regex::new(r"\.(s|S)$").unwrap();
        }
        if RE.is_match(flag.as_ref()) {
            self.is_assembly = true;
        }

        self
    }

    pub fn output_file<S>(&mut self, _flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.output_filename = args[0].as_ref().to_string();
        self
    }

    pub fn object_file<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        let val = flag.as_ref();
        self.object_files.push(val.to_string());
        self.link_args.push(val.to_string());
        self
    }

    pub fn linker_group<S>(&mut self, _start: S, count: usize, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        let group: Vec<String> = args[0..count]
            .iter()
            .map(|x| x.as_ref().to_string())
            .collect();
        self.link_args.extend(group);
        self
    }

    pub fn preprocess_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_preprocess_only = true;
        self
    }

    pub fn dependency_only<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_dependency_only = true;
        self.compile_args.push(flag.as_ref().to_string());
        self
    }

    pub fn print_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_print_only = true;
        self
    }

    pub fn assemble_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_assemble_only = true;
        self
    }

    pub fn verbose<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_verbose = true;
        self
    }

    pub fn compile_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_compile_only = true;
        self
    }

    pub fn emit_llvm<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_emit_llvm = true;
        self.is_compile_only = true;
        self
    }

    pub fn lto<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // enable Link Time Optimization
        self.is_lto = true;
        self
    }

    pub fn link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.link_args.push(flag.as_ref().to_string());
        self
    }

    pub fn compile_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self
    }

    pub fn warning_link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // NOTE: the flag cannot be used with this tool
        self.forbidden_flags.push(flag.as_ref().to_string());
        self
    }

    pub fn default_binary<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // NOTE: do nothing
        self
    }

    pub fn dependency_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self.compile_args.push(args[0].as_ref().to_string());
        self.is_dependency_only = true;
        self
    }

    pub fn compile_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self.compile_args.push(args[0].as_ref().to_string());
        self
    }

    pub fn link_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.link_args.push(flag.as_ref().to_string());
        self.link_args.push(args[0].as_ref().to_string());
        self
    }

    pub fn compile_link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());

        self.link_args.push(flag.as_ref().to_string());

        self
    }

    pub fn compile_link_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self.compile_args.push(args[0].as_ref().to_string());

        self.link_args.push(flag.as_ref().to_string());
        self.link_args.push(args[0].as_ref().to_string());

        self
    }

    fn consume_params<S>(&mut self, i: usize, arg: S, arg_info: &ArgInfo<S>, args: &[S]) -> usize
    where
        S: AsRef<str>,
    {
        let handler = arg_info.handler;
        // Exclude the current argument
        let param_start = i + 1;
        let param_end = param_start + arg_info.arity;
        let params = &args[param_start..param_end];
        handler(self, arg, params);

        arg_info.arity
    }

    pub fn parse_args<S>(&mut self, args: &[S]) -> Result<&'_ mut Self, Error>
    where
        S: AsRef<str>,
    {
        let args: Vec<String> = args.iter().map(|x| x.as_ref().to_string()).collect();
        self.input_args = args.clone();

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            // Consume the current argument, by default
            let mut offset = 1;

            // Try to match the flag exactly
            if let Some(arg_info) = ARG_EXACT_MATCH_MAP.get(arg.as_str()) {
                // Consume more parameters
                offset += self.consume_params(i, arg.to_string(), arg_info, &args);
            } else if arg == "-Wl,--start-group" {
                // Need to handle the N-ary grouping flag
                if let Some(group_end) = args[i..].iter().position(|x| x == "-Wl,--end-group") {
                    // Consume more parameters
                    offset += group_end;

                    // Need to consume the group, including both start and end
                    // group markers
                    let params = &args[i..(i + offset)];

                    self.linker_group(arg.to_string(), group_end + 1, params);
                } else {
                    // Failed to find "-Wl,--end-group"
                    // Only consume the current argument "-Wl,--start-group"
                    self.compile_unary(arg, &[]);
                }
            } else {
                // Try to match a pattern
                let mut matched = false;
                for arg_pattern in ARG_PATTERNS.iter() {
                    let pattern = &arg_pattern.pattern;
                    let arg_info = &arg_pattern.arg_info;
                    if pattern.is_match(arg.as_str()) {
                        // Consume more parameters
                        offset += self.consume_params(i, arg.to_string(), arg_info, &args);

                        matched = true;
                        break;
                    }
                }
                if !matched {
                    let handler = if is_object_file(arg)? {
                        CompilerArgsInfo::object_file
                    } else {
                        // Failed to recognize the compiler flag
                        CompilerArgsInfo::compile_unary
                    };
                    handler(self, arg, &[]);
                }
            }

            i += offset;
        }

        Ok(self)
    }
}

impl CompilerArgsInfo {
    pub fn input_args(&self) -> &Vec<String> {
        self.input_args.as_ref()
    }

    pub fn input_files(&self) -> &Vec<String> {
        self.input_files.as_ref()
    }

    pub fn object_files(&self) -> &Vec<String> {
        self.object_files.as_ref()
    }

    pub fn output_filename(&self) -> &str {
        self.output_filename.as_ref()
    }

    pub fn compile_args(&self) -> &Vec<String> {
        self.compile_args.as_ref()
    }

    pub fn link_args(&self) -> &Vec<String> {
        self.link_args.as_ref()
    }

    pub fn forbidden_flags(&self) -> &Vec<String> {
        self.forbidden_flags.as_ref()
    }

    pub fn is_verbose(&self) -> bool {
        self.is_verbose
    }

    pub fn is_dependency_only(&self) -> bool {
        self.is_dependency_only
    }

    pub fn is_preprocess_only(&self) -> bool {
        self.is_preprocess_only
    }

    pub fn is_assemble_only(&self) -> bool {
        self.is_assemble_only
    }

    pub fn is_assembly(&self) -> bool {
        self.is_assembly
    }

    pub fn is_compile_only(&self) -> bool {
        self.is_compile_only
    }

    pub fn is_emit_llvm(&self) -> bool {
        self.is_emit_llvm
    }

    pub fn is_lto(&self) -> bool {
        self.is_lto
    }

    pub fn is_print_only(&self) -> bool {
        self.is_print_only
    }

    pub fn is_bitcode_generation_skipped(&self) -> bool {
        let mut is_skipped = false;
        let mut message = "no reason";

        let conditions = [
            (
                self.input_files.is_empty(),
                "the list of input files is empty",
            ),
            (
                self.is_emit_llvm,
                "the compiler will generate bitcode in emit-llvm mode",
            ),
            (
                self.is_lto,
                "the compiler will generate bitcode during the link-time optimization",
            ),
            (
                self.is_assembly,
                "the input file(s) are written in assembly",
            ),
            (
                self.is_assemble_only,
                "we are only assembling, so cannot embed the path of the bitcode",
            ),
            (
                self.is_dependency_only && !self.is_compile_only,
                "we are only computing dependencies",
            ),
            (self.is_preprocess_only, "we are only preprocessing"),
            (
                self.is_print_only,
                "we are in print-only mode, so cannot embed the path of the bitcode",
            ),
        ];

        for (condition, reason) in conditions {
            if condition {
                is_skipped = true;
                message = reason;
            }
        }

        if is_skipped {
            log::warn!("Skip bitcode generation: {}", message);
        }

        is_skipped
    }

    pub fn mode(&self) -> CompileMode {
        let mut mode = CompileMode::Compiling;
        if self.input_files().is_empty() && self.link_args().len() > 0 {
            mode = CompileMode::Linking;
            if self.is_lto() {
                mode = CompileMode::LTO;
            }
        }

        mode
    }

    pub fn artifact_filepaths(&self) -> Result<Vec<(PathBuf, PathBuf, PathBuf)>, Error> {
        let mut artifacts = vec![];
        for src_file in &self.input_files {
            // Obtain the absolute filepath
            let src_filepath = PathBuf::from(src_file).canonicalize()?;

            // Add artifacts
            let (object_filepath, bitcode_filepath) =
                derive_object_and_bitcode_filepath(&src_filepath, self.is_compile_only)?;
            artifacts.push((src_filepath, object_filepath, bitcode_filepath));
        }

        Ok(artifacts)
    }
}
