//! The llvm-sys walk. The only module in the crate containing `unsafe`.

/// Version of the LLVM this binary links, from the C API.
pub fn llvm_version() -> String {
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    // SAFETY: LLVMGetVersion writes three unsigned values and nothing else.
    unsafe { llvm_sys::core::LLVMGetVersion(&mut major, &mut minor, &mut patch) };
    format!("{major}.{minor}.{patch}")
}
