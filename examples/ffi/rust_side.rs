unsafe extern "C" {
    fn c_offset(x: i32) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_scale(x: i32) -> i32 {
    unsafe { c_offset(x * 2) }
}
