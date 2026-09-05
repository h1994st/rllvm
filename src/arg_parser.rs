//! Command-line argument parser

use crate::{
    config::try_rllvm_config,
    constants::{arg_exact_match_map, arg_patterns, is_object_file_name},
    diagnostics::print_warning,
    error::Error,
    utils::*,
};
use regex::Regex;
use std::{
    env,
    path::{Path, PathBuf},
    sync::OnceLock,
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

/// Function signature for argument handlers.
pub type CallbackFn<S> = for<'a> fn(&'a mut CompilerArgsInfo, S, &[S]) -> &'a mut CompilerArgsInfo;
/// Boxed argument handler callback.
pub type Callback<S> = Box<CallbackFn<S>>;

/// Metadata for a compiler argument: its arity and handler function.
pub struct ArgInfo<S>
where
    S: AsRef<str>,
{
    /// Number of additional parameters consumed by this argument.
    pub arity: usize,
    /// Handler function invoked when the argument is matched.
    pub handler: CallbackFn<S>,
}

impl<S> ArgInfo<S>
where
    S: AsRef<str>,
{
    /// Create a new `ArgInfo` with the given arity and handler.
    pub fn new(arity: usize, handler: CallbackFn<S>) -> Self {
        Self { arity, handler }
    }
}

/// Regex-based argument pattern with associated handler metadata.
pub struct ArgPatternInfo<S>
where
    S: AsRef<str>,
{
    /// Regex pattern to match against compiler arguments.
    pub pattern: Regex,
    /// Handler metadata for this pattern.
    pub arg_info: ArgInfo<S>,
}

impl<S> ArgPatternInfo<S>
where
    S: AsRef<str>,
{
    /// Create a new `ArgPatternInfo` from a regex string, arity, and handler.
    pub fn new(pattern: &str, arity: usize, handler: CallbackFn<S>) -> Self {
        let pattern = Regex::new(pattern).unwrap();
        let arg_info = ArgInfo::new(arity, handler);
        Self { pattern, arg_info }
    }
}

impl CompilerArgsInfo {
    /// Handle an input file argument.
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

    /// Handle an output file argument (`-o`).
    pub fn output_file<S>(&mut self, _flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.output_filename = args[0].as_ref().to_string();
        self
    }

    /// Handle an object file argument and add it to link args.
    pub fn object_file<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        let val = flag.as_ref();
        self.object_files.push(val.to_string());
        self.link_args.push(val.to_string());
        self
    }

    /// Handle a linker group (`-Wl,--start-group ... -Wl,--end-group`).
    pub fn linker_group<S>(&mut self, _start: S, count: usize, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        for arg in &args[0..count] {
            let arg = arg.as_ref();
            self.link_args.push(arg.to_string());

            // A member of a group is still an object file. Recording it keeps
            // grouped input equivalent to the same file passed on its own,
            // which goes through `object_file` and lands in both lists.
            if is_object_file_name(arg) {
                self.object_files.push(arg.to_string());
            }
        }
        self
    }

    /// Handle a preprocess-only flag (`-E`).
    pub fn preprocess_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_preprocess_only = true;
        self
    }

    /// Handle a dependency-only flag (`-M`, `-MM`).
    pub fn dependency_only<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_dependency_only = true;
        self.compile_args.push(flag.as_ref().to_string());
        self
    }

    /// Handle a print-only flag (`-print-*`, `--version`).
    pub fn print_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_print_only = true;
        self
    }

    /// Handle an assemble-only flag (`-S`).
    pub fn assemble_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_assemble_only = true;
        self
    }

    /// Handle a verbose flag (`-v`).
    pub fn verbose<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_verbose = true;
        self
    }

    /// Handle a compile-only flag (`-c`).
    pub fn compile_only<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_compile_only = true;
        self
    }

    /// Handle an emit-LLVM flag (`-emit-llvm`).
    pub fn emit_llvm<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.is_emit_llvm = true;
        self.is_compile_only = true;
        self
    }

    /// Handle an LTO flag (`-flto`, `-flto=thin`).
    pub fn lto<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // enable Link Time Optimization
        self.is_lto = true;
        self
    }

    /// Handle a unary link flag (flag only, no additional parameter).
    pub fn link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.link_args.push(flag.as_ref().to_string());
        self
    }

    /// Handle a unary compile flag (flag only, no additional parameter).
    pub fn compile_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self
    }

    /// Handle a flag that is forbidden and recorded as a warning.
    pub fn warning_link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // NOTE: the flag cannot be used with this tool
        self.forbidden_flags.push(flag.as_ref().to_string());
        self
    }

    /// Handle a binary flag with no side effects (ignored).
    pub fn default_binary<S>(&mut self, _flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        // NOTE: do nothing
        self
    }

    /// Handle a binary dependency flag (flag + one parameter).
    pub fn dependency_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self.compile_args.push(args[0].as_ref().to_string());
        self.is_dependency_only = true;
        self
    }

    /// Handle a binary compile flag (flag + one parameter).
    pub fn compile_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());
        self.compile_args.push(args[0].as_ref().to_string());
        self
    }

    /// Handle a binary link flag (flag + one parameter).
    pub fn link_binary<S>(&mut self, flag: S, args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.link_args.push(flag.as_ref().to_string());
        self.link_args.push(args[0].as_ref().to_string());
        self
    }

    /// Handle a unary flag applied to both compile and link args.
    pub fn compile_link_unary<S>(&mut self, flag: S, _args: &[S]) -> &'_ mut Self
    where
        S: AsRef<str>,
    {
        self.compile_args.push(flag.as_ref().to_string());

        self.link_args.push(flag.as_ref().to_string());

        self
    }

    /// Handle a binary flag applied to both compile and link args (flag + one parameter).
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

    fn consume_params<S>(
        &mut self,
        i: usize,
        arg: S,
        arg_info: &ArgInfo<S>,
        args: &[S],
    ) -> Result<usize, Error>
    where
        S: AsRef<str>,
    {
        let handler = arg_info.handler;
        // Exclude the current argument
        let param_start = i + 1;
        let param_end = param_start + arg_info.arity;
        if param_end > args.len() {
            return Err(Error::InvalidArguments(format!(
                "'{}' expects {} parameter(s), but only {} remain",
                arg.as_ref(),
                arg_info.arity,
                args.len() - param_start
            )));
        }
        let params = &args[param_start..param_end];
        handler(self, arg, params);

        Ok(arg_info.arity)
    }

    /// Parse a sequence of compiler arguments and classify them.
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
                offset += self.consume_params(i, arg.to_string(), arg_info, &args)?;
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
                // Try to match a pattern. One `RegexSet` pass replaces a test
                // per pattern; the table returns the first declared match, so
                // the ordering the patterns rely on still holds.
                if let Some(arg_info) = arg_patterns().first_match(arg.as_str()) {
                    // Consume more parameters
                    offset += self.consume_params(i, arg.to_string(), arg_info, &args)?;
                } else {
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
    /// Returns the original input arguments.
    pub fn input_args(&self) -> &Vec<String> {
        self.input_args.as_ref()
    }

    /// Returns the list of input source files.
    pub fn input_files(&self) -> &Vec<String> {
        self.input_files.as_ref()
    }

    /// Returns the list of object files.
    pub fn object_files(&self) -> &Vec<String> {
        self.object_files.as_ref()
    }

    /// Returns the output filename.
    pub fn output_filename(&self) -> &str {
        self.output_filename.as_ref()
    }

    /// Returns the compilation-phase arguments.
    pub fn compile_args(&self) -> &Vec<String> {
        self.compile_args.as_ref()
    }

    /// Returns the link-phase arguments.
    pub fn link_args(&self) -> &Vec<String> {
        self.link_args.as_ref()
    }

    /// Returns flags that are forbidden for this tool.
    pub fn forbidden_flags(&self) -> &Vec<String> {
        self.forbidden_flags.as_ref()
    }

    /// Returns `true` if verbose mode is enabled.
    pub fn is_verbose(&self) -> bool {
        self.is_verbose
    }

    /// Returns `true` if only dependency generation was requested.
    pub fn is_dependency_only(&self) -> bool {
        self.is_dependency_only
    }

    /// Returns `true` if only preprocessing was requested.
    pub fn is_preprocess_only(&self) -> bool {
        self.is_preprocess_only
    }

    /// Returns `true` if only assembly output was requested.
    pub fn is_assemble_only(&self) -> bool {
        self.is_assemble_only
    }

    /// Returns `true` if the input files are assembly.
    pub fn is_assembly(&self) -> bool {
        self.is_assembly
    }

    /// Returns `true` if compile-only mode is enabled (`-c`).
    pub fn is_compile_only(&self) -> bool {
        self.is_compile_only
    }

    /// Returns `true` if LLVM IR emission is enabled (`-emit-llvm`).
    pub fn is_emit_llvm(&self) -> bool {
        self.is_emit_llvm
    }

    /// Returns `true` if link-time optimization is enabled.
    pub fn is_lto(&self) -> bool {
        self.is_lto
    }

    /// Returns `true` if print-only mode is enabled.
    pub fn is_print_only(&self) -> bool {
        self.is_print_only
    }

    /// Returns `true` if bitcode generation should be skipped for the current arguments.
    pub fn is_bitcode_generation_skipped(&self) -> Result<bool, Error> {
        /// Whether a skipped build is worth putting in front of the user.
        enum Report {
            /// Routine. Either nothing reaches the link, or the invocation
            /// never had a translation unit to compile. Reporting these would
            /// put a warning in front of every preprocess, dependency scan and
            /// link, which is how warnings stop being read.
            Quiet,
            /// The build still produces objects, they still get linked, and
            /// none of them carries a bitcode path. Extraction from the result
            /// fails, and nothing else says so until it does.
            Loud,
        }

        let conditions = [
            (
                try_rllvm_config()?.is_configure_only(),
                "we are in configure-only mode",
                Report::Quiet,
            ),
            (
                self.input_files.is_empty(),
                "the list of input files is empty",
                Report::Quiet,
            ),
            (
                self.is_emit_llvm,
                "the compiler will generate bitcode in emit-llvm mode",
                Report::Quiet,
            ),
            (
                self.is_lto,
                "the compiler will generate bitcode during the link-time optimization",
                Report::Loud,
            ),
            (
                self.is_assembly,
                "the input file(s) are written in assembly",
                Report::Quiet,
            ),
            (
                self.is_assemble_only,
                "we are only assembling, so cannot embed the path of the bitcode",
                Report::Quiet,
            ),
            (
                self.is_dependency_only && !self.is_compile_only,
                "we are only computing dependencies",
                Report::Quiet,
            ),
            (
                self.is_preprocess_only,
                "we are only preprocessing",
                Report::Quiet,
            ),
            (
                self.is_print_only,
                "we are in print-only mode, so cannot embed the path of the bitcode",
                Report::Quiet,
            ),
        ];

        for (condition, reason, report) in conditions {
            if condition {
                tracing::warn!("Skip bitcode generation: {}", reason);

                // Deliberately not a `tracing::warn!`: the default log level is
                // ERROR, so a log record is invisible to exactly the
                // non-interactive build-system runs that most need to know the
                // output cannot be extracted from later.
                if matches!(report, Report::Loud) {
                    print_warning(&format!(
                        "Bitcode generation is skipped because {reason}. \
                         The linked output carries no bitcode paths, so \
                         `rllvm-get-bc` will find nothing in it."
                    ));
                }

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Determine the current compile mode based on parsed arguments.
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

    /// Derive (source, object, bitcode) filepath triples for all input files.
    pub fn artifact_filepaths(&self) -> Result<Vec<(PathBuf, PathBuf, PathBuf)>, Error> {
        // Artifacts follow the output. With no `-o` the compiler writes into
        // the working directory, so that is the output directory too.
        let output_dir = if self.output_filename.is_empty() {
            env::current_dir()?
        } else {
            let output_filepath = PathBuf::from(&self.output_filename);
            let output_filepath = if output_filepath.is_absolute() {
                output_filepath
            } else {
                env::current_dir()?.join(output_filepath)
            };
            output_filepath
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(env::current_dir()?)
        };

        let mut artifacts = vec![];
        for src_file in &self.input_files {
            // Obtain the absolute filepath
            let src_filepath = PathBuf::from(src_file).canonicalize()?;

            // Derive filepaths of artifacts
            let (mut object_filepath, mut bitcode_filepath) = derive_object_and_bitcode_filepath(
                &src_filepath,
                &output_dir,
                self.is_compile_only,
            )?;

            // In compile-only mode an explicit `-o` names the object file the
            // compiler actually wrote, and that is the file the bitcode path
            // has to be embedded into. (`clang -c` rejects `-o` for more than
            // one input, so a single output filename is unambiguous here.)
            if self.is_compile_only && !self.output_filename.is_empty() {
                let explicit_output = PathBuf::from(&self.output_filename);
                object_filepath = if explicit_output.is_absolute() {
                    explicit_output
                } else {
                    env::current_dir()?.join(explicit_output)
                };
            }

            // Update the bitcode filepath, if the bitcode store path is provided
            if let Some(bitcode_store_path) = try_rllvm_config()?.bitcode_store_path() {
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
                        tracing::warn!(
                            "Cannot obtain the bitcode filename: {:?}",
                            bitcode_filepath
                        );
                    }
                } else {
                    tracing::warn!(
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

    fn parse_and_assert<F>(input: &str, check_func: F)
    where
        F: Fn(&CompilerArgsInfo) -> bool,
    {
        let mut args_info = CompilerArgsInfo::default();
        let args: Vec<&str> = input.split_ascii_whitespace().collect();
        let ret = args_info.parse_args(&args);
        assert!(ret.is_ok());
        assert!(check_func(ret.unwrap()));
    }

    fn assert_lto(input: &str) {
        parse_and_assert(input, |args| args.is_lto());
    }

    #[test]
    fn parsing_lto() {
        let input = r#"-pthread -c -Wno-unused-result -Wsign-compare -Wunreachable-code -DNDEBUG -g -fwrapv -O3 -Wall -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -flto -g -std=c99 -Wextra -Wno-unused-result -Wno-unused-parameter -Wno-missing-field-initializers -Wstrict-prototypes -Werror=implicit-function-declaration -fprofile-instr-use=code.profclangd -I./Include/internal  -I. -I./Include -D_FORTIFY_SOURCE=2 -D_FORTIFY_SOURCE=2 -fPIC -DPy_BUILD_CORE -DSOABI='"cpython-38-x86_64-linux-gnu"'	-o Python/dynload_shlib.o ./Python/dynload_shlib.c"#;
        assert_lto(input);

        let input = r#"-pthread -c -Wno-unused-result -Wsign-compare -Wunreachable-code -DNDEBUG -g -fwrapv -O3 -Wall -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -march=x86-64 -mtune=generic -O3 -pipe -fno-plt -g -fdebug-prefix-map=/home/legend/makepkgs/python/src=/usr/src/debug -fno-semantic-interposition -flto=thin -g -std=c99 -Wextra -Wno-unused-result -Wno-unused-parameter -Wno-missing-field-initializers -Wstrict-prototypes -Werror=implicit-function-declaration -fprofile-instr-use=code.profclangd -I./Include/internal  -I. -I./Include -D_FORTIFY_SOURCE=2 -D_FORTIFY_SOURCE=2 -fPIC -DPy_BUILD_CORE -DSOABI='"cpython-38-x86_64-linux-gnu"'	-o Python/dynload_shlib.o ./Python/dynload_shlib.c"#;
        assert_lto(input);
    }

    #[test]
    fn parsing_objective_c_sources() {
        // Objective-C sources are compilation inputs, not unrecognized flags;
        // otherwise no bitcode is generated for them.
        parse_and_assert("-c foo.m", |args| {
            args.input_files() == &["foo.m".to_string()]
        });
        parse_and_assert("-c foo.mm", |args| {
            args.input_files() == &["foo.mm".to_string()]
        });
    }

    fn assert_link_arg_count(input: &str, expected: usize) {
        parse_and_assert(input, |args| args.link_args().len() == expected);
    }

    #[test]
    fn parsing_prefers_the_first_declared_pattern() {
        // Several patterns overlap, and the earlier declaration must win:
        // `^-fsanitize=.+$` and `^-fuse-ld=.+$` both come before the catch-all
        // `^-f.+$`, which would otherwise swallow them as plain compile flags.
        // A single RegexSet pass reports every match, so this pins the choice.

        // -fsanitize=: compile *and* link, not compile-only.
        parse_and_assert("-fsanitize=address", |args| {
            args.compile_args() == &["-fsanitize=address".to_string()]
                && args.link_args() == &["-fsanitize=address".to_string()]
        });

        // -fuse-ld=: link-only.
        parse_and_assert("-fuse-ld=lld", |args| {
            args.link_args() == &["-fuse-ld=lld".to_string()] && args.compile_args().is_empty()
        });

        // A plain -f flag falls through to the catch-all: compile-only.
        parse_and_assert("-fPIC", |args| {
            args.compile_args() == &["-fPIC".to_string()] && args.link_args().is_empty()
        });
    }

    #[test]
    fn parsing_linker_group_registers_object_files() {
        // A file inside a group must be classified the same as outside one.
        let grouped = "-Wl,--start-group 7.o 8.o -lfoo @rsp.txt -Wl,--end-group";
        parse_and_assert(grouped, |args| {
            args.object_files() == &["7.o".to_string(), "8.o".to_string()]
        });

        // Everything in the span still reaches the linker, objects included:
        // both markers, two objects, a library, and a response file.
        parse_and_assert(grouped, |args| args.link_args().len() == 6);

        // Non-object members must not be mistaken for objects.
        parse_and_assert(grouped, |args| {
            !args
                .object_files()
                .iter()
                .any(|f| f == "-lfoo" || f == "@rsp.txt")
        });

        // The ungrouped form is the reference behavior.
        parse_and_assert("7.o 8.o", |args| {
            args.object_files() == &["7.o".to_string(), "8.o".to_string()]
        });
    }

    #[test]
    fn parsing_link_args() {
        let input = r#"-Wl,--fatal-warnings -Wl,--build-id=sha1 -fPIC -Wl,-z,noexecstack -Wl,-z,relro -Wl,-z,now -Wl,-z,defs -Wl,--as-needed -fuse-ld=lld -Wl,--icf=all -Wl,--color-diagnostics -flto=thin -Wl,--thinlto-jobs=8 -Wl,--thinlto-cache-dir=thinlto-cache -Wl,--thinlto-cache-policy,cache_size=10\%:cache_size_bytes=10g:cache_size_files=100000 -Wl,--lto-O0 -fwhole-program-vtables -Wl,--no-call-graph-profile-sort -m64 -Wl,-O2 -Wl,--gc-sections -Wl,--gdb-index -rdynamic -fsanitize=cfi-vcall -fsanitize=cfi-icall -pie -Wl,--disable-new-dtags -Wl,-O1,--sort-common,--as-needed,-z,relro,-z,now -o "./brotli" -Wl,--start-group @"./brotli.rsp"  -Wl,--end-group  -latomic -ldl -lpthread -lrt"#;
        assert_link_arg_count(input, 32);

        let input = r#"1.c 2.c 3.c 4.c 5.c -Wl,--start-group 7.o 8.o 9.o -Wl,--end-group 10.c 11.c 12.c 13.c"#;
        assert_link_arg_count(input, 5);
    }

    #[test]
    fn parsing_dead_strip_reaches_the_linker() {
        // Dead stripping used to be dropped, because the embedded section was
        // unreferenced and ld discarded it. The section now carries
        // `S_ATTR_NO_DEAD_STRIP`, so the flag is passed through as written.
        let input = r#"-O2 -dead_strip -Wl,-dead_strip -o prog main.c"#;
        parse_and_assert(input, |args| {
            args.forbidden_flags().is_empty()
                && args.link_args().iter().any(|x| x == "-dead_strip")
                && args.link_args().iter().any(|x| x == "-Wl,-dead_strip")
        });

        // Nothing else in the default tables is forbidden.
        let input = r#"-O2 -o prog main.c"#;
        parse_and_assert(input, |args| args.forbidden_flags().is_empty());
    }

    #[test]
    fn parsing_mode_flags() {
        parse_and_assert("-E main.c", |a| a.is_preprocess_only());
        parse_and_assert("-S main.c", |a| a.is_assemble_only());
        parse_and_assert("-c main.c", |a| a.is_compile_only());
        parse_and_assert("-emit-llvm -c main.c", |a| a.is_emit_llvm());
        parse_and_assert("-v -c main.c", |a| a.is_verbose());
        parse_and_assert("-M main.c", |a| a.is_dependency_only());
        parse_and_assert("-MM main.c", |a| a.is_dependency_only());

        // Absence must not set them.
        parse_and_assert("-o prog main.c", |a| {
            !a.is_preprocess_only()
                && !a.is_assemble_only()
                && !a.is_compile_only()
                && !a.is_emit_llvm()
                && !a.is_verbose()
                && !a.is_dependency_only()
                && !a.is_lto()
                && !a.is_print_only()
        });
    }

    #[test]
    fn parsing_assembly_input_is_detected() {
        parse_and_assert("-c foo.s", |a| a.is_assembly());
        parse_and_assert("-c foo.S", |a| a.is_assembly());
        parse_and_assert("-c foo.c", |a| !a.is_assembly());
    }

    #[test]
    fn parsing_output_filename() {
        parse_and_assert("-o prog main.c", |a| a.output_filename() == "prog");
        parse_and_assert("-c main.c", |a| a.output_filename().is_empty());
    }

    #[test]
    fn parsing_input_and_object_files() {
        parse_and_assert("-o prog a.c b.c", |a| {
            a.input_files() == &["a.c", "b.c"] && a.object_files().is_empty()
        });
        // input_args records the original argument list verbatim.
        parse_and_assert("-c main.c", |a| a.input_args() == &["-c", "main.c"]);
    }

    #[test]
    fn parsing_binary_flags_consume_their_argument() {
        // Arity drives consumption, so a binary flag must not leak its value
        // into the input file list -- that is the classic failure here.
        parse_and_assert("-I include -o prog main.c", |a| {
            a.input_files() == &["main.c"] && a.compile_args().iter().any(|x| x == "include")
        });
        parse_and_assert("-include hdr.h -c main.c", |a| {
            a.input_files() == &["main.c"]
        });
        parse_and_assert("-MF deps.d -c main.c", |a| a.input_files() == &["main.c"]);
        parse_and_assert("-e entry -o prog main.c", |a| {
            a.input_files() == &["main.c"]
        });
        parse_and_assert("-arch arm64 -o prog main.c", |a| {
            a.input_files() == &["main.c"]
        });
    }

    #[test]
    fn parsing_splits_compile_and_link_arguments() {
        // -I and -O2 are compile concerns; -L and -l are link concerns. The
        // split matters because compile_args feeds bitcode generation and
        // link_args feeds the relink.
        parse_and_assert("-I inc -L lib -lfoo -O2 -o prog main.c", |a| {
            a.compile_args() == &["-I", "inc", "-O2"] && a.link_args() == &["-L", "lib", "-lfoo"]
        });
    }

    #[test]
    fn parsing_warning_flags_reach_both_phases() {
        parse_and_assert("-Wall -o prog main.c", |a| {
            a.compile_args().iter().any(|x| x == "-Wall")
        });
    }

    #[test]
    fn parsing_unrecognised_flag_is_kept_not_dropped() {
        // The fallback must stay total: an unknown flag is treated as a compile
        // flag rather than raising, because it is asked about every argument.
        parse_and_assert("-fsome-future-flag -c main.c", |a| {
            a.compile_args().iter().any(|x| x == "-fsome-future-flag")
                && a.input_files() == &["main.c"]
        });
    }

    #[test]
    fn parsing_defines_are_compile_arguments() {
        parse_and_assert("-DFOO=1 -UBAR -c main.c", |a| {
            a.compile_args().iter().any(|x| x == "-DFOO=1")
                && a.compile_args().iter().any(|x| x == "-UBAR")
        });
    }

    #[test]
    fn parsing_sysroot_reaches_both_phases() {
        // --sysroot is the one flag routed through compile_link_binary: it and
        // its argument must appear in both phases.
        parse_and_assert("--sysroot /sdk -o prog main.c", |a| {
            a.compile_args() == &["--sysroot", "/sdk"] && a.link_args() == &["--sysroot", "/sdk"]
        });
    }

    #[test]
    fn parsing_empty_argument_list() {
        parse_and_assert("", |a| {
            a.input_files().is_empty() && a.object_files().is_empty() && !a.is_compile_only()
        });
    }
}
