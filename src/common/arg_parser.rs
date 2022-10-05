//! Command-line argument information

use lazy_static::lazy_static;
use regex::Regex;

use crate::common::{
    constants::{ARG_EXACT_MATCH_MAP, ARG_PATTERNS},
    utils::*,
    Error,
};

/// Compiler argument information
#[derive(Debug, Default)]
pub struct CompilerArgsInfo {
    pub input_list: Vec<String>,
    pub input_files: Vec<String>,
    pub object_files: Vec<String>,
    pub output_filename: String,
    pub compile_args: Vec<String>,
    pub link_args: Vec<String>,
    pub forbidden_flags: Vec<String>,
    pub is_verbose: bool,
    pub is_dependency_only: bool,
    pub is_preprocess_only: bool,
    pub is_assemble_only: bool,
    pub is_assembly: bool,
    pub is_compile_only: bool,
    pub is_emit_llvm: bool,
    pub is_lto: bool,
    pub is_print_only: bool,
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
        self.input_list = args.clone();

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
                    let handler = if is_object_file(arg) {
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
