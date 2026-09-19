use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use object::Object;
use rllvm::{
    cli::ExtractionArgs, config::try_rllvm_config, error::Error, merge::MergeStrategy, utils::*,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

pub fn run() -> Result<(), Error> {
    let args = ExtractionArgs::parse();

    if args.output_dir.is_some()
        || !args.selection.is_empty()
        || matches!(
            InputKind::from_path(&args.input)?,
            InputKind::Bitcode | InputKind::JsonObject
        )
    {
        return extract_catalog(&args);
    }

    // Set log level
    // The verbose flag will override the configured log level
    let log_level = if args.verbose == 0 {
        try_rllvm_config()?.log_level()
    } else {
        match args.verbose {
            1 => Level::WARN,
            2 => Level::INFO,
            3 => Level::DEBUG,
            _ => Level::TRACE,
        }
    };
    // Diagnostics belong on stderr: these wrappers stand in for a compiler, and
    // anything on stdout is captured as build output (`-E` preprocessing,
    // `-print-*` queries), where a log line corrupts the result.
    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_writer(std::io::stderr)
        .init();

    // Check if the input file exists
    let input = &args.input;
    // The path goes into the error, not only into this log line: the message
    // reaches the user, the log line may not.
    let input_filepath = input.canonicalize().map_err(|err| {
        tracing::error!(
            "Failed to obtain the absolute filepath of the input: input={:?}, err={}",
            input,
            err
        );
        Error::file(input, err)
    })?;
    if !input_filepath.exists() {
        let error_message = format!("Input file does not exist: {:?}", input_filepath);
        tracing::error!("{}", error_message);
        return Err(Error::MissingFile(error_message));
    }
    tracing::info!("Input file: {:?}", input_filepath);

    // Parse object file(s)
    let input_data = fs::read(&input_filepath).map_err(|err| {
        tracing::error!(
            "Failed to read the input file: input_filepath={:?}, err={}",
            input_filepath,
            err
        );
        Error::file(&input_filepath, err)
    })?;
    let mut object_files = vec![];
    // Resolve merge strategy: --merge-strategy takes precedence, then -b flag, then default (Full).
    let strategy = match args.merge_strategy {
        Some(s) => s,
        None if args.build_bitcode_archive => MergeStrategy::Archive,
        None => MergeStrategy::Full,
    };

    let mut output_file_ext = match strategy {
        MergeStrategy::Archive => "bca",
        _ => "bc",
    };

    if let Ok(input_object_file) = object::File::parse(&*input_data) {
        tracing::info!("Input object file kind: {:?}", input_object_file.kind());
        object_files = vec![input_object_file];
    } else if let Ok(input_archive_file) = object::read::archive::ArchiveFile::parse(&*input_data) {
        tracing::info!("Input archive file kind: {:?}", input_archive_file.kind());

        for member in input_archive_file.members() {
            let member = member.inspect_err(|err| {
                tracing::error!("Failed to obtain the archive member: err={}", err);
            })?;
            let member_name = String::from_utf8_lossy(member.name());
            tracing::info!("{}", member_name);
            let member_object_data = member.data(&*input_data).inspect_err(|err| {
                tracing::error!(
                    "Failed to read the object data of the archive member: member={}, err={}",
                    member_name,
                    err
                );
            })?;
            let object_file = object::File::parse(member_object_data).inspect_err(|err| {
                tracing::error!(
                    "Failed to parse the object data of the archive member: member={}, err={}",
                    member_name,
                    err
                );
            })?;
            object_files.push(object_file)
        }

        // For archive inputs, adjust extension unless already set to bca by Archive strategy.
        if strategy != MergeStrategy::Archive {
            output_file_ext = "a.bc";
        }
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
            tracing::error!(
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
        tracing::error!("{}", error_message);
        return Err(Error::MissingFile(error_message));
    }
    // Resolve relative entries against the root. A leading separator means the
    // entry is absolute -- the historical format -- and is left alone, which is
    // what lets both forms coexist in one section without a version marker.
    let bitcode_filepaths: Vec<PathBuf> = {
        let root = args
            .bitcode_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        bitcode_filepaths
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            })
            .collect()
    };
    tracing::debug!("Bitcode filepaths: {:?}", bitcode_filepaths);
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
            tracing::error!(
                "Failed to save the manifest file: manifest_filepath={:?}, err={}",
                manifest_filepath,
                err
            );
            err
        })?;
        tracing::info!("Save manifest: {:?}", manifest_filepath);
    }

    // Merge bitcode files using the selected strategy
    if let Some(code) =
        rllvm::merge::merge_bitcode_files(strategy, &bitcode_filepaths, output_filepath.clone())
            .map_err(|err| {
                tracing::error!(
                    "Failed to merge ({}) bitcode files: bitcode_filepaths={:?}, err={:?}",
                    strategy,
                    bitcode_filepaths,
                    err
                );
                err
            })?
        && code != 0
    {
        std::process::exit(code);
    }
    tracing::info!("Output file: {:?}", output_filepath);

    Ok(())
}

fn extract_catalog(args: &ExtractionArgs) -> Result<(), Error> {
    use rllvm::catalog::{copy_modules, inventory, select_modules};
    let root = args.bitcode_root.as_deref().unwrap_or(Path::new("."));
    let catalog = inventory(&args.input, root, None)?;
    let selected = select_modules(
        &catalog,
        &args.selection.selection(),
        &std::env::current_dir()?,
    )?;
    if let Some(directory) = &args.output_dir {
        copy_modules(&selected, directory)?;
        return Ok(());
    }
    let strategy = args
        .merge_strategy
        .unwrap_or(if args.build_bitcode_archive {
            MergeStrategy::Archive
        } else {
            MergeStrategy::Full
        });
    if strategy != MergeStrategy::Archive {
        let targets: std::collections::BTreeSet<_> = selected
            .modules
            .iter()
            .filter_map(|m| m.target_triple.as_deref())
            .collect();
        let layouts: std::collections::BTreeSet<_> = selected
            .modules
            .iter()
            .filter_map(|m| m.data_layout.as_deref())
            .collect();
        if targets.len() > 1 || layouts.len() > 1 {
            return Err(Error::InvalidArguments("selected modules have incompatible targets/data layouts; select a compatible subset or use archive output".into()));
        }
    }
    if args.save_manifest && selected.modules.iter().any(|m| m.archive_member.is_some()) {
        return Err(Error::InvalidArguments("a path manifest cannot represent embedded archive members; use --output-dir for a portable catalog".into()));
    }
    let stem = args
        .input
        .file_stem()
        .ok_or_else(|| Error::InvalidArguments("input has no filename".into()))?
        .to_string_lossy();
    let output = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "{stem}.{}",
            if strategy == MergeStrategy::Archive {
                "bca"
            } else {
                "bc"
            }
        ))
    });
    let input = args.input.canonicalize()?;
    let manifest = if args.save_manifest {
        let filename = output
            .file_name()
            .ok_or_else(|| Error::InvalidArguments("output has no filename".into()))?
            .to_string_lossy();
        Some(
            input
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("{filename}.manifest")),
        )
    } else {
        None
    };
    for destination in std::iter::once(&output).chain(manifest.iter()) {
        if let Ok(existing) = destination.canonicalize()
            && (existing == input
                || selected
                    .modules
                    .iter()
                    .filter_map(|m| m.path.as_ref())
                    .any(|path| path.canonicalize().ok().as_ref() == Some(&existing)))
        {
            return Err(Error::InvalidArguments(
                "output or manifest would overwrite an input artifact; choose another -o path"
                    .into(),
            ));
        }
    }
    let scratch = tempfile::tempdir()?;
    let directory = scratch.path().join("modules");
    let copied = copy_modules(&selected, &directory)?;
    let paths: Vec<PathBuf> = copied
        .modules
        .iter()
        .map(|m| directory.join(m.path.as_ref().expect("copied module has a path")))
        .collect();
    if rllvm::merge::merge_bitcode_files(strategy, &paths, output.clone())?
        .is_some_and(|code| code != 0)
    {
        return Err(Error::ExecutionFailure(
            "selected module merge failed".into(),
        ));
    }
    if let Some(manifest) = manifest {
        let contents = selected
            .modules
            .iter()
            .filter_map(|m| m.path.as_ref())
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(manifest, contents)?;
    }
    Ok(())
}

pub fn main() -> std::process::ExitCode {
    rllvm::error::report(run())
}
