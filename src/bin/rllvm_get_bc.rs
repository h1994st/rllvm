use std::{fs, path::PathBuf};

use clap::Parser;
use log::LevelFilter;
use object::Object;
use rllvm::{error::Error, utils::*};
use simple_logger::SimpleLogger;

/// Extraction arguments
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-get-bc",
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

    /// Build bitcode archive (only used for archive files, e.g., *.a)
    #[arg(short = 'b', long)]
    build_bitcode_archive: bool,

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
    log::info!("Input file: {:?}", input_filepath);

    // Parse object file(s)
    let input_data = fs::read(&input_filepath)?;
    let mut object_files = vec![];
    let mut output_file_ext = "bc";
    let mut build_bitcode_archive = false;
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

        if args.build_bitcode_archive {
            output_file_ext = "bca";
        } else {
            output_file_ext = "a.bc";
        }
        build_bitcode_archive = args.build_bitcode_archive;
    } else {
        return Err(Error::Unknown("Unsupported file format".to_string()));
    };

    // Obtain the output filepath
    let input_filename = input_filepath.file_stem().unwrap().to_string_lossy();
    let output_filepath = args.output.unwrap_or(PathBuf::from(format!(
        "{}.{}",
        input_filename, output_file_ext
    )));

    // Extract bitcode filepaths
    let bitcode_filepaths = extract_bitcode_filepaths_from_parsed_objects(&object_files)?;
    if args.save_manifest {
        // Write bitcode filepaths into the manifest file
        let input_parent_dir = input_filepath.parent().unwrap();
        let output_filename = output_filepath.file_name().unwrap();
        let manifest_filepath =
            input_parent_dir.join(format!("{}.manifest", output_filename.to_string_lossy()));

        let manifest_contents = bitcode_filepaths
            .iter()
            .map(|bitcode_filepath| bitcode_filepath.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&manifest_filepath, manifest_contents)?;
        log::info!("Save manifest: {:?}", manifest_filepath);
    }

    // Link or archive bitcode files
    if build_bitcode_archive {
        log::info!("Archive bitcode files");
        if let Some(code) = archive_bitcode_files(&bitcode_filepaths, output_filepath.clone())? {
            if code != 0 {
                std::process::exit(code);
            }
        }
    } else {
        log::info!("Link bitcode files");
        if let Some(code) = link_bitcode_files(&bitcode_filepaths, output_filepath.clone())? {
            if code != 0 {
                std::process::exit(code);
            }
        }
    }
    log::info!("Output file: {:?}", output_filepath);

    Ok(())
}
