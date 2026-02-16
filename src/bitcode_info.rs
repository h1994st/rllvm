//! Bitcode file analysis using `llvm-dis`.
//!
//! Parses disassembled LLVM IR to extract module-level metadata (target triple,
//! data layout) and per-function statistics (basic block and instruction counts).

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    error::Error,
    utils::{execute_command_for_stdout_string, find_llvm_config},
};

/// Information about a single function in the bitcode module.
#[derive(Debug)]
pub struct FunctionInfo {
    pub name: String,
    pub basic_block_count: usize,
    pub instruction_count: usize,
}

/// Aggregated analysis of an LLVM bitcode file.
#[derive(Debug)]
pub struct BitcodeInfo {
    pub file_path: PathBuf,
    pub file_size: u64,
    pub target_triple: Option<String>,
    pub data_layout: Option<String>,
    pub functions: Vec<FunctionInfo>,
    pub total_basic_blocks: usize,
    pub total_instructions: usize,
}

/// Locate the `llvm-dis` binary by deriving it from `llvm-config --bindir`.
pub fn find_llvm_dis() -> Result<PathBuf, Error> {
    let llvm_config = find_llvm_config()?;
    let bindir = execute_command_for_stdout_string(&llvm_config, &["--bindir"])?;
    let llvm_dis = PathBuf::from(bindir).join("llvm-dis");
    if !llvm_dis.exists() {
        return Err(Error::MissingFile(format!(
            "llvm-dis not found at {}",
            llvm_dis.display()
        )));
    }
    Ok(llvm_dis)
}

/// Disassemble a bitcode file to LLVM IR text using `llvm-dis`.
fn disassemble(llvm_dis: &Path, bc_path: &Path) -> Result<String, Error> {
    execute_command_for_stdout_string(
        llvm_dis,
        &[bc_path.as_os_str(), "-o".as_ref(), "-".as_ref()],
    )
}

/// Parse disassembled LLVM IR text into [`BitcodeInfo`].
fn parse_ir(ir: &str, file_path: PathBuf, file_size: u64) -> BitcodeInfo {
    let mut target_triple = None;
    let mut data_layout = None;
    let mut functions: Vec<FunctionInfo> = Vec::new();

    // Parsing state
    let mut current_func_name: Option<String> = None;
    let mut current_bb_count: usize = 0;
    let mut current_instr_count: usize = 0;

    for line in ir.lines() {
        let trimmed = line.trim();

        // Module-level attributes
        if let Some(rest) = trimmed.strip_prefix("target triple = \"") {
            if let Some(value) = rest.strip_suffix('"') {
                target_triple = Some(value.to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("target datalayout = \"") {
            if let Some(value) = rest.strip_suffix('"') {
                data_layout = Some(value.to_string());
            }
            continue;
        }

        // Function definition start: "define ... @name(...) {"
        if trimmed.starts_with("define ") {
            if let Some(name) = extract_function_name(trimmed) {
                // Close any previous function (shouldn't happen with well-formed IR)
                if let Some(prev_name) = current_func_name.take() {
                    functions.push(FunctionInfo {
                        name: prev_name,
                        basic_block_count: current_bb_count,
                        instruction_count: current_instr_count,
                    });
                }
                current_func_name = Some(name);
                current_bb_count = 0;
                current_instr_count = 0;
                // The entry block is implicit (first label after define)
                // We count it when we see the first instruction or label
                continue;
            }
        }

        // Inside a function body
        if current_func_name.is_some() {
            // End of function
            if trimmed == "}" {
                if let Some(name) = current_func_name.take() {
                    functions.push(FunctionInfo {
                        name,
                        basic_block_count: current_bb_count,
                        instruction_count: current_instr_count,
                    });
                }
                continue;
            }

            // Basic block label: "label:" or "N:" at the start of the line (not indented)
            // In LLVM IR, labels are not indented and end with ':'
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
                current_bb_count += 1;
                continue;
            }

            // Instructions are indented lines that are not comments or empty
            if (line.starts_with(' ') || line.starts_with('\t'))
                && !trimmed.is_empty()
                && !trimmed.starts_with(';')
            {
                current_instr_count += 1;
                // The first instruction implicitly starts the entry basic block
                if current_bb_count == 0 {
                    current_bb_count = 1;
                }
            }
        }
    }

    let total_basic_blocks = functions.iter().map(|f| f.basic_block_count).sum();
    let total_instructions = functions.iter().map(|f| f.instruction_count).sum();

    BitcodeInfo {
        file_path,
        file_size,
        target_triple,
        data_layout,
        functions,
        total_basic_blocks,
        total_instructions,
    }
}

/// Extract the function name from a `define` line.
///
/// Looks for `@name(` pattern in the line.
fn extract_function_name(line: &str) -> Option<String> {
    let at_pos = line.find('@')?;
    let after_at = &line[at_pos + 1..];
    let end = after_at.find('(')?;
    let name = &after_at[..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Analyze a bitcode file and return structured information.
///
/// Locates `llvm-dis`, disassembles the bitcode, and parses the resulting IR.
pub fn analyze_bitcode(bc_path: &Path) -> Result<BitcodeInfo, Error> {
    let llvm_dis = find_llvm_dis()?;
    let file_size = fs::metadata(bc_path).map_err(Error::Io)?.len();
    let ir = disassemble(&llvm_dis, bc_path)?;
    Ok(parse_ir(&ir, bc_path.to_path_buf(), file_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_function_name() {
        assert_eq!(
            extract_function_name("define i32 @main(i32 %argc, i8** %argv) {"),
            Some("main".to_string())
        );
        assert_eq!(
            extract_function_name("define internal void @helper() #0 {"),
            Some("helper".to_string())
        );
        assert_eq!(
            extract_function_name("declare void @llvm.dbg.value(...)"),
            None
        );
    }

    #[test]
    fn test_parse_ir_basic() {
        let ir = r#"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx15.0.0"

define i32 @foo(i32 %x) {
entry:
  %add = add i32 %x, 1
  ret i32 %add
}

define void @bar() {
entry:
  ret void
}
"#;
        let info = parse_ir(ir, PathBuf::from("test.bc"), 1024);
        assert_eq!(
            info.target_triple.as_deref(),
            Some("arm64-apple-macosx15.0.0")
        );
        assert_eq!(
            info.data_layout.as_deref(),
            Some("e-m:o-i64:64-i128:128-n32:64-S128-Fn32")
        );
        assert_eq!(info.functions.len(), 2);
        assert_eq!(info.functions[0].name, "foo");
        assert_eq!(info.functions[0].basic_block_count, 1);
        assert_eq!(info.functions[0].instruction_count, 2);
        assert_eq!(info.functions[1].name, "bar");
        assert_eq!(info.functions[1].basic_block_count, 1);
        assert_eq!(info.functions[1].instruction_count, 1);
        assert_eq!(info.total_basic_blocks, 2);
        assert_eq!(info.total_instructions, 3);
    }

    #[test]
    fn test_parse_ir_multiple_blocks() {
        let ir = r#"
target triple = "x86_64-unknown-linux-gnu"

define i32 @branch(i1 %cond) {
entry:
  br i1 %cond, label %then, label %else

then:
  ret i32 1

else:
  ret i32 0
}
"#;
        let info = parse_ir(ir, PathBuf::from("test.bc"), 512);
        assert_eq!(info.functions.len(), 1);
        assert_eq!(info.functions[0].name, "branch");
        assert_eq!(info.functions[0].basic_block_count, 3);
        assert_eq!(info.functions[0].instruction_count, 3);
    }
}
