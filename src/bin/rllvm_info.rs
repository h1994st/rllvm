use std::{fs, path::PathBuf};

use clap::Parser;
use owo_colors::OwoColorize;
use rllvm::{
    bitcode_info::{BitcodeInfo, analyze_bitcode},
    error::Error,
    utils::extract_bitcode_filepaths_from_object_file,
};

/// Analyze LLVM bitcode files
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-info",
    about = "Display information about LLVM bitcode files",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
struct InfoArgs {
    /// Input file (bitcode .bc or object file with embedded bitcode)
    input: PathBuf,

    /// List all function names
    #[arg(short = 'f', long)]
    functions: bool,
}

/// Detect whether a file is an LLVM bitcode file by checking its magic bytes.
fn is_bitcode_file(path: &PathBuf) -> Result<bool, Error> {
    let data = fs::read(path)?;
    // LLVM bitcode files start with 'BC' (0x42, 0x43) magic
    Ok(data.len() >= 2 && data[0] == 0x42 && data[1] == 0x43)
}

/// Try to parse as an object file to check for embedded bitcode.
fn try_extract_bitcode_from_object(path: &PathBuf) -> Result<Option<PathBuf>, Error> {
    let data = fs::read(path)?;
    if object::File::parse(&*data).is_ok() {
        let bc_paths = extract_bitcode_filepaths_from_object_file(path)?;
        if let Some(first) = bc_paths.into_iter().next() {
            if first.exists() {
                return Ok(Some(first));
            }
        }
    }
    Ok(None)
}

fn print_info(info: &BitcodeInfo, show_functions: bool) {
    println!("{}", "=== Bitcode Info ===".bold());
    println!("File         : {}", info.file_path.display());
    println!("File size    : {} bytes", info.file_size);
    if let Some(triple) = &info.target_triple {
        println!("Target triple: {}", triple);
    }
    if let Some(layout) = &info.data_layout {
        println!("Data layout  : {}", layout);
    }
    println!("Functions    : {}", info.functions.len());
    println!("Basic blocks : {}", info.total_basic_blocks);
    println!("Instructions : {}", info.total_instructions);

    if show_functions && !info.functions.is_empty() {
        println!();
        println!("{}", "=== Functions ===".bold());
        for func in &info.functions {
            println!(
                "  {} (blocks: {}, instructions: {})",
                func.name.green(),
                func.basic_block_count,
                func.instruction_count,
            );
        }
    }
}

fn main() -> Result<(), Error> {
    let args = InfoArgs::parse();

    let input = &args.input;
    let input_path = input
        .canonicalize()
        .map_err(|e| Error::MissingFile(format!("Cannot resolve input path {:?}: {}", input, e)))?;

    // Determine the bitcode file to analyze
    let bc_path = if is_bitcode_file(&input_path)? {
        input_path
    } else {
        // Try extracting from an object file
        match try_extract_bitcode_from_object(&input_path)? {
            Some(path) => path,
            None => {
                return Err(Error::InvalidArguments(format!(
                    "{} is not a bitcode file and no embedded bitcode was found",
                    input.display()
                )));
            }
        }
    };

    let info = analyze_bitcode(&bc_path)?;
    print_info(&info, args.functions);

    Ok(())
}
