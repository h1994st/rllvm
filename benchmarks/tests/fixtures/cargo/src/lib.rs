#[unsafe(no_mangle)]
pub extern "C" fn project_first() -> i32 { helper() }
#[inline(never)]
pub fn helper() -> i32 { 42 }
