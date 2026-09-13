use clap::Parser;
use rllvm::{
    catalog::ModuleStatus,
    cli::{CompdbArgs, CompdbCommand},
    compilation_database::{CompilationDatabase, GenerateOptions},
    error::Error,
};

fn run() -> Result<bool, Error> {
    match CompdbArgs::parse().command {
        CompdbCommand::List { input } => {
            let catalog = CompilationDatabase::load(&input)?.list();
            serde_json::to_writer_pretty(std::io::stdout().lock(), &catalog)
                .map_err(|error| Error::InvalidArguments(error.to_string()))?;
            println!();
            Ok(true)
        }
        CompdbCommand::Generate {
            input,
            output_dir,
            source,
            entry,
            extra_arg,
            jobs,
        } => {
            let catalog = CompilationDatabase::load(&input)?.generate(&GenerateOptions {
                output_dir: output_dir.clone(),
                sources: source,
                entries: entry,
                extra_arguments: extra_arg,
                jobs: jobs as usize,
            })?;
            let available = catalog
                .modules
                .iter()
                .filter(|module| module.status == ModuleStatus::Available)
                .count();
            eprintln!(
                "Generated {available}/{} selected modules; catalog: {}",
                catalog.modules.len(),
                output_dir.join("catalog.json").display()
            );
            for module in &catalog.modules {
                if module.status != ModuleStatus::Available {
                    eprintln!("{}: {}", module.id, module.diagnostics.join("; "));
                }
            }
            Ok(available == catalog.modules.len())
        }
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
