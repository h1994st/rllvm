use clap::Parser;
use rllvm::cli::QueryArgs;

fn main() {
    let args = QueryArgs::parse();
    if args.llvm_version {
        println!("{}", rllvm::query::llvm_version());
    }
}
