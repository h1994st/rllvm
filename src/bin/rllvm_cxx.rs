use rllvm::{compiler_wrapper::CompilerKind, error::Error};

pub mod rllvm_cc;

pub fn main() -> Result<(), Error> {
    rllvm_cc::rllvm_main("rllvm++", CompilerKind::ClangXX)
}
