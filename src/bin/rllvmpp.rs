use std::env;

use rllvm::compiler_wrapper::{llvm::ClangWrapper, CompilerKind, CompilerWrapper};

pub fn main() {
    let args: Vec<String> = env::args().collect();
    // Skip the first argument
    let args = &args[1..];

    let mut cc = ClangWrapper::new("rllvm++", CompilerKind::ClangXX);

    if let Some(code) = cc
        .parse_args(&args)
        .expect("Failed to parse arguments")
        .run()
        .expect("Failed to run the wrapped compiler")
    {
        std::process::exit(code);
    }
}
