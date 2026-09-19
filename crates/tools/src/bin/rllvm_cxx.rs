use rllvm::compiler_wrapper::CompilerKind;

pub mod rllvm_cc;

pub fn main() -> std::process::ExitCode {
    rllvm::error::report(rllvm_cc::rllvm_main("rllvm++", CompilerKind::ClangXX))
}
