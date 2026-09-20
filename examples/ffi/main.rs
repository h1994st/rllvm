unsafe extern "C" {
    fn c_double(x: i32) -> i32;
    fn cxx_triple(x: i32) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    let doubled = unsafe { c_double(21) };
    let tripled = unsafe { cxx_triple(doubled) };
    assert_eq!(doubled, 42);
    assert_eq!(tripled, 126);
    println!("{doubled} {tripled}");
}
