//! Command-line argument parser

use crate::{
    config::rllvm_config,
    constants::{arg_exact_match_map, arg_patterns},
    error::Error,
    utils::*,
};
use regex::Regex;
use std::{path::PathBuf, sync::OnceLock};

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
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"\.(s|S)$").unwrap());
        if re.is_match(flag.as_ref()) {
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
            if let Some(arg_info) = arg_exact_match_map().get(arg.as_str()) {
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
                for arg_pattern in arg_patterns().iter() {
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
    pub fn input_args(&self) -> &[String] {
        &self.input_args
    }

    pub fn input_files(&self) -> &[String] {
        &self.input_files
    }

    pub fn object_files(&self) -> &[String] {
        &self.object_files
    }

    pub fn output_filename(&self) -> &str {
        &self.output_filename
    }

    pub fn compile_args(&self) -> &[String] {
        &self.compile_args
    }

    pub fn link_args(&self) -> &[String] {
        &self.link_args
    }

    pub fn forbidden_flags(&self) -> &[String] {
        &self.forbidden_flags
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
                rllvm_config().is_configure_only(),
                "we are in configure-only mode",
            ),
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
        if self.input_files().is_empty() && !self.link_args().is_empty() {
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

            // Derive filepaths of artifacts
            let (object_filepath, mut bitcode_filepath) =
                derive_object_and_bitcode_filepath(&src_filepath, self.is_compile_only)?;

            // Update the bitcode filepath, if the bitcode store path is provided
            if let Some(bitcode_store_path) = rllvm_config().bitcode_store_path() {
                if bitcode_store_path.exists() {
                    // Obtain a new bitcode filename based on the hash of the source filepath
                    if bitcode_filepath.file_name().is_some() {
                        let src_filepath_hash = calculate_filepath_hash(&src_filepath);
                        let bitcode_file_stem =
                            bitcode_filepath.file_stem().unwrap().to_string_lossy();
                        let bitcode_file_ext =
                            bitcode_filepath.extension().unwrap().to_string_lossy();

                        let new_bitcode_filename =
                            format!("{bitcode_file_stem}_{src_filepath_hash}.{bitcode_file_ext}");

                        bitcode_filepath = bitcode_store_path.join(new_bitcode_filename);
                    } else {
                        log::warn!("Cannot obtain the bitcode filename: {:?}", bitcode_filepath);
                    }
                } else {
                    log::warn!(
                        "Ignore the bitcode store path, as it does not exist: {:?}",
                        bitcode_store_path
                    );
                }
            }
            artifacts.push((src_filepath, object_filepath, bitcode_filepath));
        }

        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests {
    use super::CompilerArgsInfo;

    fn test_parsing<F>(input: &str, check_func: F)
    where
        F: Fn(&CompilerArgsInfo) -> bool,
    {
        let mut args_info = CompilerArgsInfo::default();
        let args: Vec<&str> = input.split_ascii_whitespace().collect();
        let ret = args_info.parse_args(&args);
        assert!(ret.is_ok());
        assert!(check_func(ret.unwrap()));
    }

    fn test_parsing_lto_internal(input: &str) {
        test_parsing(input, |args| args.is_lto());
    }

    #[test]
    fn test_parsing_lto() {
        let input = r#"-pthread -c -Wno-unused-result -Wsign-compare -Wunreachable-code -DNDEBUG -g -fwrapv -O3 -Wall -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -flto -g -std=c99 -Wextra -Wno-unused-result -Wno-unused-parameter -Wno-missing-field-initializers -Wstrict-prototypes -Werror=implicit-function-declaration -fprofile-instr-use=code.profclangd -I./Include/internal  -I. -I./Include -D_FORTIFY_SOURCE=2 -D_FORTIFY_SOURCE=2 -fPIC -DPy_BUILD_CORE -DSOABI='"cpython-38-x86_64-linux-gnu"'	-o Python/dynload_shlib.o ./Python/dynload_shlib.c"#;
        test_parsing_lto_internal(input);

        let input = r#"-pthread -c -Wno-unused-result -Wsign-compare -Wunreachable-code -DNDEBUG -g -fwrapv -O3 -Wall -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -flto=thin -g -std=c99 -Wextra -Wno-unused-result -Wno-unused-parameter -Wno-missing-field-initializers -Wstrict-prototypes -Werror=implicit-function-declaration -fprofile-instr-use=code.profclangd -I./Include/internal  -I. -I./Include -D_FORTIFY_SOURCE=2 -D_FORTIFY_SOURCE=2 -fPIC -DPy_BUILD_CORE -DSOABI='"cpython-38-x86_64-linux-gnu"'	-o Python/dynload_shlib.o ./Python/dynload_shlib.c"#;
        test_parsing_lto_internal(input);
    }

    fn test_parsing_link_args_internal(input: &str, expected: usize) {
        test_parsing(input, |args| args.link_args().len() == expected);
    }

    #[test]
    fn test_parsing_link_args() {
        let input = r#"-Wl,--fatal-warnings -Wl,--build-id=sha1 -fPIC -Wl,-z,noexecstack -Wl,-z,relro -Wl,-z,now -Wl,-z,defs -Wl,--as-needed -fuse-ld=lld -Wl,--icf=all -Wl,--color-diagnostics -flto=thin -Wl,--thinlto-jobs=8 -Wl,--thinlto-cache-dir=thinlto-cache -Wl,--thinlto-cache-policy,cache_size=10\%:cache_size_bytes=10g:cache_size_files=100000 -Wl,--lto-O0 -fwhole-program-vtables -Wl,--no-call-graph-profile-sort -m64 -Wl,-O2 -Wl,--gc-sections -Wl,--gdb-index -rdynamic -fsanitize=cfi-vcall -fsanitize=cfi-icall -pie -Wl,--disable-new-dtags -Wl,-O1,--sort-common,--as-needed,-z,relro,-z,now -o "./brotli" -Wl,--start-group @"./brotli.rsp"  -Wl,--end-group  -latomic -ldl -lpthread -lrt"#;
        test_parsing_link_args_internal(input, 32);

        let input = r#"1.c 2.c 3.c 4.c 5.c -Wl,--start-group 7.o 8.o 9.o -Wl,--end-group 10.c 11.c 12.c 13.c"#;
        test_parsing_link_args_internal(input, 5);
    }
}
