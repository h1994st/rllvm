//! Internal constants for argument parsing, platform-specific section names,
//! and LLVM version ranges.
//!
//! This module is `pub(crate)` and not part of the public API.

use std::{collections::HashMap, sync::OnceLock};

use crate::arg_parser::{ArgInfo, ArgPatternInfo, CompilerArgsInfo};

type CallbackMap = HashMap<&'static str, ArgInfo<String>>;
type PatternCallbackVec = Vec<ArgPatternInfo<String>>;

/// Mach-O segment name for the embedded bitcode section on Darwin.
pub const DARWIN_SEGMENT_NAME: &str = "__RLLVM";

/// Mach-O section name for the embedded bitcode on Darwin.
pub const DARWIN_SECTION_NAME: &str = "__llvm_bc";

/// ELF section name for the embedded bitcode.
pub const ELF_SECTION_NAME: &str = ".llvm_bc";

/// Environment variable name for overriding the rllvm config file path.
pub const DEFAULT_RLLVM_CONF_FILEPATH_ENV_NAME: &str = "RLLVM_CONFIG";

/// Environment variable name for the user's home directory.
pub const HOME_ENV_NAME: &str = "HOME";

/// The default filepath of the configuration file under `$HOME`.
pub const DEFAULT_CONF_FILEPATH_UNDER_HOME: &str = ".rllvm/config.toml";

/// The max version of `LLVM` we're looking for
#[cfg(not(target_vendor = "apple"))]
pub const LLVM_VERSION_MAX: u32 = 33;

/// The min version of `LLVM` we're looking for
#[cfg(not(target_vendor = "apple"))]
pub const LLVM_VERSION_MIN: u32 = 6;

/// Returns the lazily-initialized map of exact compiler flag matches to their handlers.
pub fn arg_exact_match_map() -> &'static CallbackMap {
    static ARG_EXACT_MATCH_MAP: OnceLock<CallbackMap> = OnceLock::new();

    ARG_EXACT_MATCH_MAP.get_or_init(|| {
        let mut m = HashMap::new();

        m.insert("/dev/null", ArgInfo::new(0, CompilerArgsInfo::input_file));

        m.insert("-", ArgInfo::new(0, CompilerArgsInfo::print_only));
        m.insert("-o", ArgInfo::new(1, CompilerArgsInfo::output_file));
        m.insert("-c", ArgInfo::new(0, CompilerArgsInfo::compile_only));
        m.insert("-E", ArgInfo::new(0, CompilerArgsInfo::preprocess_only));
        m.insert("-S", ArgInfo::new(0, CompilerArgsInfo::assemble_only));

        m.insert("--verbose", ArgInfo::new(0, CompilerArgsInfo::verbose));
        m.insert("--param", ArgInfo::new(1, CompilerArgsInfo::default_binary));
        m.insert(
            "-aux-info",
            ArgInfo::new(1, CompilerArgsInfo::default_binary),
        );

        m.insert("--version", ArgInfo::new(0, CompilerArgsInfo::compile_only));
        m.insert("-v", ArgInfo::new(0, CompilerArgsInfo::compile_only));

        m.insert("-w", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-W", ArgInfo::new(0, CompilerArgsInfo::compile_unary));

        m.insert("-emit-llvm", ArgInfo::new(0, CompilerArgsInfo::emit_llvm));
        m.insert("-flto", ArgInfo::new(0, CompilerArgsInfo::lto));

        m.insert("-pipe", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-undef", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-nostdinc",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-nostdinc++",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-Qunused-arguments",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-no-integrated-as",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-integrated-as",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-no-canonical-prefixes",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );

        m.insert(
            "--sysroot",
            ArgInfo::new(1, CompilerArgsInfo::compile_link_binary),
        );

        m.insert(
            "-no-cpp-precomp",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );

        m.insert("-pthread", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert(
            "-nostdlibinc",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );

        m.insert(
            "-mno-omit-leaf-frame-pointer",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-maes", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mno-aes", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mavx", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mno-avx", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mavx2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-avx2",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mno-red-zone",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-mmmx", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mbmi", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mbmi2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mf161c", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mfma", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mno-mmx", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-global-merge",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mno-80387",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-msse", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-mno-sse", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-msse2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-sse2",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-msse3", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-sse3",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-mssse3", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-ssse3",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-msse4", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-sse4",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-msse4.1", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-sse4.1",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-msse4.2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-sse4.2",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-msoft-float",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-m3dnow", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-mno-3dnow",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-m16",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "-m32",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "-m64",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "-mstackrealign",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mretpoline-external-thunk",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mno-fp-ret-in-387",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mskip-rax-setup",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-mindirect-branch-register",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );

        m.insert("-mllvm", ArgInfo::new(1, CompilerArgsInfo::compile_binary));

        m.insert("-A", ArgInfo::new(1, CompilerArgsInfo::compile_binary));
        m.insert("-D", ArgInfo::new(1, CompilerArgsInfo::compile_binary));
        m.insert("-U", ArgInfo::new(1, CompilerArgsInfo::compile_binary));

        m.insert("-arch", ArgInfo::new(1, CompilerArgsInfo::compile_binary));

        m.insert("-P", ArgInfo::new(1, CompilerArgsInfo::compile_unary));
        m.insert("-C", ArgInfo::new(1, CompilerArgsInfo::compile_unary));

        m.insert("-M", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MM", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MF", ArgInfo::new(1, CompilerArgsInfo::dependency_binary));
        m.insert("-MJ", ArgInfo::new(1, CompilerArgsInfo::dependency_binary));
        m.insert("-MG", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MP", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MT", ArgInfo::new(1, CompilerArgsInfo::dependency_binary));
        m.insert("-MQ", ArgInfo::new(1, CompilerArgsInfo::dependency_binary));
        m.insert("-MD", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MV", ArgInfo::new(0, CompilerArgsInfo::dependency_only));
        m.insert("-MMD", ArgInfo::new(0, CompilerArgsInfo::dependency_only));

        m.insert("-I", ArgInfo::new(1, CompilerArgsInfo::compile_binary));
        m.insert(
            "-idirafter",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-include",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-imacros",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-iprefix",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-iwithprefix",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-iwithprefixbefore",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-isystem",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert(
            "-isysroot",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );
        m.insert("-iquote", ArgInfo::new(1, CompilerArgsInfo::compile_binary));
        m.insert(
            "-imultilib",
            ArgInfo::new(1, CompilerArgsInfo::compile_binary),
        );

        m.insert("-ansi", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-pedantic",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert("-x", ArgInfo::new(1, CompilerArgsInfo::compile_binary));

        m.insert("-g", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-g0", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-g1", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-g2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-g3", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-ggdb", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-ggdb0", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-ggdb1", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-ggdb2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-ggdb3", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-gdwarf", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert(
            "-gdwarf-2",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-gdwarf-3",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-gdwarf-4",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-gline-tables-only",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-grecord-gcc-switches",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-ggnu-pubnames",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );

        m.insert("-p", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-pg", ArgInfo::new(0, CompilerArgsInfo::compile_unary));

        m.insert("-O", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-O0", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-O1", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-O2", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-O3", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-Os", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-Ofast", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-Og", ArgInfo::new(0, CompilerArgsInfo::compile_unary));
        m.insert("-Oz", ArgInfo::new(0, CompilerArgsInfo::compile_unary));

        m.insert("-Xclang", ArgInfo::new(1, CompilerArgsInfo::compile_binary));
        m.insert(
            "-Xpreprocessor",
            ArgInfo::new(1, CompilerArgsInfo::default_binary),
        );
        m.insert(
            "-Xassembler",
            ArgInfo::new(1, CompilerArgsInfo::default_binary),
        );
        m.insert(
            "-Xlinker",
            ArgInfo::new(1, CompilerArgsInfo::default_binary),
        );

        m.insert("-l", ArgInfo::new(1, CompilerArgsInfo::link_binary));
        m.insert("-L", ArgInfo::new(1, CompilerArgsInfo::link_binary));
        m.insert("-T", ArgInfo::new(1, CompilerArgsInfo::link_binary));
        m.insert("-u", ArgInfo::new(1, CompilerArgsInfo::link_binary));
        m.insert(
            "-install_name",
            ArgInfo::new(1, CompilerArgsInfo::link_binary),
        );

        m.insert("-e", ArgInfo::new(1, CompilerArgsInfo::link_binary));
        m.insert("-rpath", ArgInfo::new(1, CompilerArgsInfo::link_binary));

        m.insert("-shared", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert("-static", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert(
            "-static-libgcc",
            ArgInfo::new(0, CompilerArgsInfo::link_unary),
        );
        m.insert("-pie", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert("-nostdlib", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert(
            "-nodefaultlibs",
            ArgInfo::new(0, CompilerArgsInfo::link_unary),
        );
        m.insert("-rdynamic", ArgInfo::new(0, CompilerArgsInfo::link_unary));

        m.insert("-dynamiclib", ArgInfo::new(0, CompilerArgsInfo::link_unary));
        m.insert(
            "-current_version",
            ArgInfo::new(1, CompilerArgsInfo::link_binary),
        );
        m.insert(
            "-compatibility_version",
            ArgInfo::new(1, CompilerArgsInfo::link_binary),
        );

        m.insert(
            "-print-multi-directory",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-print-multi-lib",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-print-libgcc-file-name",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );
        m.insert(
            "-print-search-dirs",
            ArgInfo::new(0, CompilerArgsInfo::compile_unary),
        );

        m.insert(
            "-fprofile-arcs",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "-coverage",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "--coverage",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );
        m.insert(
            "-fopenmp",
            ArgInfo::new(0, CompilerArgsInfo::compile_link_unary),
        );

        m.insert(
            "-Wl,-dead_strip",
            ArgInfo::new(0, CompilerArgsInfo::warning_link_unary),
        );
        m.insert(
            "-dead_strip",
            ArgInfo::new(0, CompilerArgsInfo::warning_link_unary),
        );

        m
    })
}

/// Returns the lazily-initialized list of regex-based compiler flag patterns and their handlers.
pub fn arg_patterns() -> &'static PatternCallbackVec {
    static ARG_PATTERNS: OnceLock<PatternCallbackVec> = OnceLock::new();
    ARG_PATTERNS.get_or_init(|| {
        vec![
            ArgPatternInfo::new(r"^-MF.*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-MJ.*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-MQ.*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-MT.*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-Wl,.+$", 0, CompilerArgsInfo::link_unary),
            ArgPatternInfo::new(r"^-W[^l].*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-W[l][^,].*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-(l|L).+$", 0, CompilerArgsInfo::link_unary),
            ArgPatternInfo::new(r"^-I.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-D.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-B.+$", 0, CompilerArgsInfo::compile_link_unary),
            ArgPatternInfo::new(r"^-isystem.+$", 0, CompilerArgsInfo::compile_link_unary),
            ArgPatternInfo::new(r"^-U.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-fsanitize=.+$", 0, CompilerArgsInfo::compile_link_unary),
            ArgPatternInfo::new(r"^-fuse-ld=.+$", 0, CompilerArgsInfo::link_unary),
            ArgPatternInfo::new(r"^-flto=.+$", 0, CompilerArgsInfo::lto),
            ArgPatternInfo::new(r"^-f.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-rtlib=.+$", 0, CompilerArgsInfo::link_unary),
            ArgPatternInfo::new(r"^-std=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-stdlib=.+$", 0, CompilerArgsInfo::compile_link_unary),
            ArgPatternInfo::new(r"^-mtune=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^--sysroot=.+$", 0, CompilerArgsInfo::compile_link_unary),
            ArgPatternInfo::new(r"^-print-.*$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(
                r"^-mmacosx-version-min=.+$",
                0,
                CompilerArgsInfo::compile_link_unary,
            ),
            ArgPatternInfo::new(
                r"^-mstack-alignment=.+$",
                0,
                CompilerArgsInfo::compile_unary,
            ),
            ArgPatternInfo::new(r"^-march=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-mregparm=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(r"^-mcmodel=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(
                r"^-mpreferred-stack-boundary=.+$",
                0,
                CompilerArgsInfo::compile_unary,
            ),
            ArgPatternInfo::new(
                r"^-mindirect-branch=.+$",
                0,
                CompilerArgsInfo::compile_unary,
            ),
            ArgPatternInfo::new(r"^--param=.+$", 0, CompilerArgsInfo::compile_unary),
            ArgPatternInfo::new(
                r"^.+\.(c|cc|cpp|C|cxx|i|s|S|bc)$",
                0,
                CompilerArgsInfo::input_file,
            ),
            ArgPatternInfo::new(
                r"^.+\.([fF](|[0-9][0-9]|or|OR|pp|PP))$",
                0,
                CompilerArgsInfo::input_file,
            ),
            ArgPatternInfo::new(
                r"^.+\.(o|lo|So|so|po|a|dylib|pico|nossppico)$",
                0,
                CompilerArgsInfo::object_file,
            ),
            ArgPatternInfo::new(r"^.+\.dylib(\.\d)+$", 0, CompilerArgsInfo::object_file),
            ArgPatternInfo::new(r"^.+\.(So|so)(\.\d)+$", 0, CompilerArgsInfo::object_file),
        ]
    })
}
