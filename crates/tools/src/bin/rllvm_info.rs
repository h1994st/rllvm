use std::{fs, path::PathBuf};

use clap::Parser;
use owo_colors::OwoColorize;
use rllvm::cli::InfoArgs;
use rllvm_core::{
    bitcode_info::{BitcodeInfo, analyze_bitcode},
    error::Error,
    utils::{InputKind, extract_bitcode_filepaths_from_parsed_object, print_stdout},
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

fn format_info(info: &BitcodeInfo, show_functions: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", "=== Bitcode Info ===".bold()));
    out.push_str(&format!("File         : {}\n", info.file_path.display()));
    out.push_str(&format!("File size    : {} bytes\n", info.file_size));
    if let Some(triple) = &info.target_triple {
        out.push_str(&format!("Target triple: {}\n", triple));
    }
    if let Some(layout) = &info.data_layout {
        out.push_str(&format!("Data layout  : {}\n", layout));
    }
    out.push_str(&format!("Functions    : {}\n", info.functions.len()));
    out.push_str(&format!("Basic blocks : {}\n", info.total_basic_blocks));
    out.push_str(&format!("Instructions : {}\n", info.total_instructions));

    if show_functions && !info.functions.is_empty() {
        out.push('\n');
        out.push_str(&format!("{}\n", "=== Functions ===".bold()));
        for func in &info.functions {
            out.push_str(&format!(
                "  {} (blocks: {}, instructions: {})\n",
                func.name.green(),
                func.basic_block_count,
                func.instruction_count,
            ));
        }
    }
    out
}

fn run() -> Result<(), Error> {
    let args = InfoArgs::parse();

    if args.json {
        let root = args
            .bitcode_root
            .as_deref()
            .unwrap_or(std::path::Path::new("."));
        let catalog = rllvm_core::catalog::inventory(&args.input, root, None)?;
        let selected = rllvm_core::catalog::select_modules(
            &catalog,
            &args.selection.selection(),
            &std::env::current_dir()?,
        )?;
        let json = serde_json::to_string_pretty(&selected)
            .map_err(|error| Error::InvalidArguments(error.to_string()))?;
        print_stdout(&format!("{json}\n"))?;
        if selected
            .modules
            .iter()
            .any(|module| module.status != rllvm_core::catalog::ModuleStatus::Available)
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
    print_stdout(&format_info(&info, args.functions))?;

    Ok(())
}

fn main() -> std::process::ExitCode {
    rllvm_core::error::report(run())
}
