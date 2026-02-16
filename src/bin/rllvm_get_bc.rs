use std::{fs, path::PathBuf};

use clap::Parser;
use log::LevelFilter;
use object::Object;
use rllvm::{config::rllvm_config, error::Error, utils::*};
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
    // The verbose flag will override the configured log level
    let log_level = if args.verbose == 0 {
        rllvm_config().log_level().to_level_filter()
    } else {
        LevelFilter::iter()
            .nth(1 + args.verbose as usize)
            .unwrap_or(LevelFilter::max())
    };
    if let Err(err) = SimpleLogger::new().with_level(log_level).init() {
        let error_message = format!("Failed to set the logger: err={}", err);
        log::error!("{}", error_message);
        return Err(Error::LoggerError(error_message));
    }

    // Check if the input file exists
    let input = &args.input;
    let input_filepath = input.canonicalize().map_err(|err| {
        log::error!(
            "Failed to obtain the absolute filepath of the input: input={:?}, err={}",
            input,
            err
        );
        err
    })?;
    if !input_filepath.exists() {
        let error_message = format!("Input file does not exist: {:?}", input_filepath);
        log::error!("{}", error_message);
        return Err(Error::MissingFile(error_message));
    }
    log::info!("Input file: {:?}", input_filepath);

    // Parse object file(s)
    let input_data = fs::read(&input_filepath).map_err(|err| {
        log::error!(
            "Failed to read the input file: input_filepath={:?}, err={}",
            input_filepath,
            err
        );
        err
    })?;
    let mut object_files = vec![];
    let mut output_file_ext = "bc";
    let mut build_bitcode_archive = false;
    if let Ok(input_object_file) = object::File::parse(&*input_data) {
        log::info!("Input object file kind: {:?}", input_object_file.kind());
        object_files = vec![input_object_file];
    } else if let Ok(input_archive_file) = object::read::archive::ArchiveFile::parse(&*input_data) {
        log::info!("Input archive file kind: {:?}", input_archive_file.kind());

        for member in input_archive_file.members() {
            let member = member.inspect_err(|err| {
                log::error!("Failed to obtain the archive member: err={}", err);
            })?;
            let member_name = String::from_utf8_lossy(member.name());
            log::info!("{}", member_name);
            let member_object_data = member.data(&*input_data).inspect_err(|err| {
                log::error!(
                    "Failed to read the object data of the archive member: member={}, err={}",
                    member_name,
                    err
                );
            })?;
            let object_file = object::File::parse(member_object_data).inspect_err(|err| {
                log::error!(
                    "Failed to parse the object data of the archive member: member={}, err={}",
                    member_name,
                    err
                );
            })?;
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
    let bitcode_filepaths =
        extract_bitcode_filepaths_from_parsed_objects(&object_files).map_err(|err| {
            log::error!(
                "Failed to extract bitcode filepaths: object_files={:?}, err={:?}",
                object_files,
                err
            );
            err
        })?;
    if bitcode_filepaths.is_empty() {
        let error_message = format!(
            "No bitcode filepaths found in the input file: {:?}",
            input_filepath
        );
        log::error!("{}", error_message);
        return Err(Error::MissingFile(error_message));
    }
    log::debug!("Bitcode filepaths: {:?}", bitcode_filepaths);
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
        fs::write(&manifest_filepath, manifest_contents).map_err(|err| {
            log::error!(
                "Failed to save the manifest file: manifest_filepath={:?}, err={}",
                manifest_filepath,
                err
            );
            err
        })?;
        log::info!("Save manifest: {:?}", manifest_filepath);
    }

    // Link or archive bitcode files
    let merge_bitcode_func = if build_bitcode_archive {
        log::info!("Archive bitcode files");
        archive_bitcode_files
    } else {
        log::info!("Link bitcode files");
        link_bitcode_files
    };
    if let Some(code) =
        merge_bitcode_func(&bitcode_filepaths, output_filepath.clone()).map_err(|err| {
            let merge_action = if build_bitcode_archive {
                "archive"
            } else {
                "link"
            };
            log::error!(
                "Failed to {} bitcode files: bitcode_filepaths={:?}, err={:?}",
                merge_action,
                bitcode_filepaths,
                err
            );
            err
        })?
        && code != 0
    {
        std::process::exit(code);
    }
    log::info!("Output file: {:?}", output_filepath);

    Ok(())
}
