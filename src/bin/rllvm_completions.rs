use std::io;

use clap::{Command, CommandFactory, Parser};
use clap_complete::generate;
use rllvm::cli::{
    BinName, ClangWrapperArgs, CompletionArgs, ExtractionArgs, InfoArgs, InitArgs, RustcWrapperArgs,
};

/// The clap `Command` a binary parses with.
///
/// Every arm comes from the definition the binary itself uses, so completions
/// cannot describe a flag that no longer exists -- which is what happened while
/// this generator kept its own hand-written copy of each CLI.
fn command_for(bin: BinName) -> Command {
    match bin {
        // `ClangWrapperArgs` carries no `name`: both wrappers share it, and
        // `rllvm_main` names the command after the running binary for the same
        // reason. Completions have to do that naming too, or `rllvm-cxx` gets a
        // script that defines `_rllvm-cc`.
        BinName::Cc => ClangWrapperArgs::command().name("rllvm-cc"),
        BinName::Cxx => ClangWrapperArgs::command()
            .name("rllvm-cxx")
            .about("Execute the wrapped clang++ compiler"),
        BinName::GetBc => ExtractionArgs::command(),
        BinName::Init => InitArgs::command(),
        BinName::Info => InfoArgs::command(),
        BinName::Rustc => RustcWrapperArgs::command(),
        BinName::Completions => CompletionArgs::command(),
    }
}

fn main() {
    let args = CompletionArgs::parse();

    let mut cmd = command_for(args.bin);
    let bin_name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, &bin_name, &mut io::stdout());
}
