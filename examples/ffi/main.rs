unsafe extern "C" {
    fn c_double(x: i32) -> i32;
    fn cxx_triple(x: i32) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    // 21 goes out to C, which doubles it by calling back into Rust, then out
    // to C++, which triples the result. Printing 42 and 126 is what says all
    // three languages ran, in that order, with the callback working.
    let doubled = unsafe { c_double(21) };
    let tripled = unsafe { cxx_triple(doubled) };
    println!("doubled={doubled} tripled={tripled}");
}
