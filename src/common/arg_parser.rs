//! Command-line argument parser

#[derive(Debug)]
pub struct CompilerArgInfo {
    input_list: Vec<String>,
    input_files: Vec<String>,
    object_files: Vec<String>,
    output_filename: String,
    compile_args: Vec<String>,
    link_args: Vec<String>,
    forbidden_flags: Vec<String>,
    is_verbose: bool,
    is_dependency_only: bool,
    is_preprocess_only: bool,
    is_assemble_only: bool,
    is_assembly: bool,
    is_compile_only: bool,
    is_emit_llvm: bool,
    is_lto: bool,
    is_print_only: bool,
}
