use std::{fs, path::PathBuf};

use clap::Parser;
use owo_colors::OwoColorize;
use rllvm::{
    bitcode_info::{BitcodeInfo, analyze_bitcode},
    cli::InfoArgs,
    error::Error,
    utils::{InputKind, extract_bitcode_filepaths_from_parsed_object},
};

/// Try to parse as an object file to check for embedded bitcode.
fn try_extract_bitcode_from_object(path: &PathBuf) -> Result<Option<PathBuf>, Error> {
    let data = fs::read(path).map_err(|error| Error::file(path, error))?;
    if let Ok(object) = object::File::parse(&*data) {
        let bc_paths = extract_bitcode_filepaths_from_parsed_object(&object)?;
        if let Some(first) = bc_paths.into_iter().next()
            && first.exists()
        {
            return Ok(Some(first));
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

fn run() -> Result<(), Error> {
    let args = InfoArgs::parse();

    if args.json {
        let root = args
            .bitcode_root
            .as_deref()
            .unwrap_or(std::path::Path::new("."));
        let catalog = rllvm::catalog::inventory(&args.input, root, None)?;
        let selected = rllvm::catalog::select_modules(
            &catalog,
            &args.selection.selection(),
            &std::env::current_dir()?,
        )?;
        let json = serde_json::to_string_pretty(&selected)
            .map_err(|error| Error::InvalidArguments(error.to_string()))?;
        println!("{json}");
        if selected
            .modules
            .iter()
            .any(|module| module.status != rllvm::catalog::ModuleStatus::Available)
        {
            return Err(Error::MissingFile(
                "some selected modules are unavailable; see the JSON catalog".into(),
            ));
        }
        return Ok(());
    }
    if !args.selection.is_empty() {
        return Err(Error::InvalidArguments(
            "module/source/configuration selection requires --json".into(),
        ));
    }

    let input = &args.input;
    let input_path = input.canonicalize().map_err(|e| Error::file(input, e))?;

    // Determine the bitcode file to analyze
    let bc_path = if InputKind::from_path(&input_path)? == InputKind::Bitcode {
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

fn main() -> std::process::ExitCode {
    rllvm::error::report(run())
}
