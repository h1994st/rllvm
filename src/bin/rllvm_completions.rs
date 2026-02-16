use std::io;
use std::path::PathBuf;

use clap::{Command, Parser, ValueEnum};
use clap_complete::{Shell, generate};

/// Which binary to generate completions for
#[derive(Clone, Debug, ValueEnum)]
enum BinName {
    Cc,
    Cxx,
    GetBc,
}

/// Generate shell completions for rllvm tools
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-completions",
    about = "Generate shell completions for rllvm tools",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
struct CompletionArgs {
    /// Shell to generate completions for
    #[arg(long, value_enum)]
    shell: Shell,

    /// Binary to generate completions for
    #[arg(long, value_enum, default_value = "cc")]
    bin: BinName,
}

/// Build the clap Command for rllvm-cc
fn build_rllvm_cc_cmd() -> Command {
    Command::new("rllvm-cc")
        .about("Execute the wrapped clang compiler")
        .arg(
            clap::Arg::new("compiler")
                .short('c')
                .long("compiler")
                .help("Path to the wrapped compiler")
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            clap::Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Verbose mode")
                .action(clap::ArgAction::Count),
        )
        .arg(
            clap::Arg::new("clang_args")
                .help("Compiler arguments")
                .last(true)
                .num_args(..),
        )
}

/// Build the clap Command for rllvm-cxx
fn build_rllvm_cxx_cmd() -> Command {
    // rllvm-cxx has the same flags as rllvm-cc
    build_rllvm_cc_cmd()
        .name("rllvm-cxx")
        .about("Execute the wrapped clang++ compiler")
}

/// Build the clap Command for rllvm-get-bc
fn build_rllvm_get_bc_cmd() -> Command {
    Command::new("rllvm-get-bc")
        .about("Extract a single bitcode file for the given input")
        .arg(
            clap::Arg::new("input")
                .help("Input filepath for bitcode extraction")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            clap::Arg::new("output")
                .short('o')
                .long("output")
                .help("Output filepath of the extracted bitcode file")
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            clap::Arg::new("build-bitcode-archive")
                .short('b')
                .long("build-bitcode-archive")
                .help("Build bitcode archive (only used for archive files, e.g., *.a)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("save-manifest")
                .short('m')
                .long("save-manifest")
                .help("Save manifest of all filepaths of underlying bitcode files")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Verbose mode")
                .action(clap::ArgAction::Count),
        )
}

fn main() {
    let args = CompletionArgs::parse();

    let mut cmd = match args.bin {
        BinName::Cc => build_rllvm_cc_cmd(),
        BinName::Cxx => build_rllvm_cxx_cmd(),
        BinName::GetBc => build_rllvm_get_bc_cmd(),
    };

    let bin_name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, &bin_name, &mut io::stdout());
}
