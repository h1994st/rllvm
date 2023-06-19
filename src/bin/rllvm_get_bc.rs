use std::{fs, path::PathBuf};

use clap::Parser;
use log::LevelFilter;
use object::Object;
use rllvm::{
    error::Error,
    utils::{extract_bitcode_filepaths_from_parsed_objects, link_bitcode_files},
};
use simple_logger::SimpleLogger;

/// Extraction arguments
#[derive(Parser, Debug)]
#[command(
    name = "rllvm get-bc",
    about = "Extract a single bitcode file for the given input",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
struct ExtractionArgs {
    /// Input filepath for bitcode extraction
    input: PathBuf,

    /// Output filepath of the extracted bitcode file
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Build bitcode module
    #[arg(short = 'b', long)]
    build_bitcode_module: bool,

    /// Save manifest of all filepaths of underlying bitcode files
    #[arg(short = 'm', long)]
    save_manifest: bool,

    /// Verbose mode
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    verbose: u8,
}

pub fn main() -> Result<(), Error> {
    let args = ExtractionArgs::parse();

    // Set log level
    let log_level = LevelFilter::iter()
        .nth(1 + args.verbose as usize)
        .unwrap_or(LevelFilter::max());
    SimpleLogger::new()
        .with_level(log_level)
        .init()
        .map_err(|err| Error::LoggerError(err.to_string()))?;

    // Check if the input file exists
    let input_filepath = args.input.canonicalize()?;
    if !input_filepath.exists() {
        let error_message = format!("Input file does not exist: {:?}", input_filepath);
        log::error!("{}", error_message);
        return Err(Error::MissingFile(error_message));
    }

    // Parse object file(s)
    let input_data = fs::read(&input_filepath)?;
    let mut object_files = vec![];
    let mut output_file_ext = "bc";
    if let Ok(input_object_file) = object::File::parse(&*input_data) {
        log::info!("Input object file kind: {:?}", input_object_file.kind());
        object_files = vec![input_object_file];
    } else if let Ok(input_archive_file) = object::read::archive::ArchiveFile::parse(&*input_data) {
        log::info!("Input archive file kind: {:?}", input_archive_file.kind());

        for member in input_archive_file.members() {
            let member = member?;
            log::info!("{}", String::from_utf8_lossy(member.name()));
            let object_file = object::File::parse(member.data(&*input_data)?)?;
            object_files.push(object_file)
        }

        if args.build_bitcode_module {
            output_file_ext = "a.bc";
        } else {
            output_file_ext = "bca";
        }
    } else {
        return Err(Error::Unknown("Unsupported file format".to_string()));
    };

    // Obtain the output filepath
    let input_filename = input_filepath.file_name().unwrap().to_string_lossy();
    let output_filepath = args.output.unwrap_or(PathBuf::from(format!(
        "{}.{}",
        input_filename, output_file_ext
    )));

    // Extract bitcode filepaths
    let bitcode_filepaths = extract_bitcode_filepaths_from_parsed_objects(&object_files)?;

    // Link bitcode files
    if let Some(code) = link_bitcode_files(&bitcode_filepaths, output_filepath)? {
        std::process::exit(code);
    }

    Ok(())
}
