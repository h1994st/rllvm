#[test]
fn query_binary_reports_its_llvm_major() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rllvm-query"))
        .arg("--llvm-version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("23."), "unexpected LLVM version: {text}");
}
