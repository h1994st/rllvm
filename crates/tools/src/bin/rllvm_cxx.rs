use rllvm_core::compiler_wrapper::CompilerKind;

pub mod rllvm_cc;

pub fn main() -> std::process::ExitCode {
    rllvm_core::error::report(rllvm_cc::rllvm_main("rllvm++", CompilerKind::ClangXX))
}
