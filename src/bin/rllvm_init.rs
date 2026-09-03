use std::{
    env, fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use rllvm::{
    config::config_filepath,
    error::Error,
    utils::{execute_llvm_config, find_llvm_config},
};

/// CLI arguments for rllvm-init
#[derive(Parser, Debug)]
#[command(
    name = "rllvm-init",
    about = "Auto-detect LLVM installation and generate rllvm configuration",
    author = "Shengtuo Hu <h1994st@gmail.com>",
    version
)]
struct InitArgs {
    /// Output path for the generated config file
    ///
    /// Defaults to wherever the rest of the toolchain reads its configuration
    /// from: `$RLLVM_CONFIG` when set, otherwise `~/.rllvm/config.toml`.
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// Print detected configuration without writing to disk
    #[arg(long)]
    dry_run: bool,

    /// Override LLVM installation path (directory containing bin/llvm-config)
    #[arg(long)]
    llvm_prefix: Option<PathBuf>,
}

/// Detected LLVM tool paths
struct DetectedTools {
    llvm_config: PathBuf,
    llvm_version: String,
    clang: PathBuf,
    clangxx: PathBuf,
    llvm_ar: PathBuf,
    llvm_link: PathBuf,
    /// Optional: recorded when present, but no code path invokes it today
    llvm_objcopy: Option<PathBuf>,
}

fn find_llvm_config_with_prefix(prefix: &Path) -> Result<PathBuf, Error> {
    let candidate = prefix.join("bin").join("llvm-config");
    if candidate.exists() {
        return Ok(candidate.canonicalize()?);
    }
    // Maybe the prefix IS the bin directory
    let candidate = prefix.join("llvm-config");
    if candidate.exists() {
        return Ok(candidate.canonicalize()?);
    }
    Err(Error::MissingFile(format!(
        "llvm-config not found under {:?}",
        prefix
    )))
}

fn detect_tools(llvm_prefix: Option<&Path>) -> Result<DetectedTools, Error> {
    // Step 1: Find llvm-config
    let llvm_config = if let Some(prefix) = llvm_prefix {
        eprintln!(
            "Searching for LLVM in user-specified prefix: {}",
            prefix.display()
        );
        find_llvm_config_with_prefix(prefix)?
    } else {
        eprintln!("Auto-detecting LLVM installation...");
        find_llvm_config()?
    };
    eprintln!("  Found llvm-config: {}", llvm_config.display());

    // Step 2: Get LLVM version
    let llvm_version = execute_llvm_config(&llvm_config, &["--version"])?;
    eprintln!("  LLVM version: {}", llvm_version);

    // Step 3: Get bin directory and derive tool paths
    let bindir = PathBuf::from(execute_llvm_config(&llvm_config, &["--bindir"])?);
    eprintln!("  LLVM bindir: {}", bindir.display());

    let clang = bindir.join("clang");
    let clangxx = bindir.join("clang++");
    let llvm_ar = bindir.join("llvm-ar");
    let llvm_link = bindir.join("llvm-link");
    let llvm_objcopy = bindir.join("llvm-objcopy");

    // Step 4: Check version consistency by querying clang --version
    if clang.exists() {
        match std::process::Command::new(&clang).arg("--version").output() {
            Ok(output) => {
                let clang_version_output = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = clang_version_output.lines().next() {
                    eprintln!("  clang: {}", first_line);
                }
            }
            Err(e) => eprintln!("  Warning: could not query clang version: {}", e),
        }
    }

    // Step 5: Report which tools are found / missing
    let tools: &[(&str, &Path)] = &[
        ("clang", &clang),
        ("clang++", &clangxx),
        ("llvm-ar", &llvm_ar),
        ("llvm-link", &llvm_link),
    ];

    let mut missing = Vec::new();
    for (name, path) in tools {
        if path.exists() {
            eprintln!("  {}: OK", name);
        } else {
            eprintln!("  {}: MISSING ({})", name, path.display());
            missing.push(*name);
        }
    }

    if !missing.is_empty() {
        return Err(Error::MissingFile(format!(
            "Missing LLVM tools: {}",
            missing.join(", ")
        )));
    }

    // `llvm-objcopy` is optional: it is recorded when present, but nothing
    // invokes it, so its absence is not a failure.
    let llvm_objcopy = if llvm_objcopy.exists() {
        eprintln!("  llvm-objcopy: OK");
        Some(llvm_objcopy)
    } else {
        eprintln!("  llvm-objcopy: not found (optional)");
        None
    };

    Ok(DetectedTools {
        llvm_config,
        llvm_version,
        clang,
        clangxx,
        llvm_ar,
        llvm_link,
        llvm_objcopy,
    })
}

fn generate_toml(tools: &DetectedTools) -> String {
    let mut toml_content = format!(
        r#"llvm_config_filepath = "{}"
clang_filepath = "{}"
clangxx_filepath = "{}"
llvm_ar_filepath = "{}"
llvm_link_filepath = "{}"
"#,
        tools.llvm_config.display(),
        tools.clang.display(),
        tools.clangxx.display(),
        tools.llvm_ar.display(),
        tools.llvm_link.display(),
    );

    if let Some(llvm_objcopy) = &tools.llvm_objcopy {
        toml_content.push_str(&format!(
            "llvm_objcopy_filepath = \"{}\"\n",
            llvm_objcopy.display()
        ));
    }

    toml_content
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn main() -> Result<(), Error> {
    let args = InitArgs::parse();

    let tools = detect_tools(args.llvm_prefix.as_deref())?;
    let toml_content = generate_toml(&tools);

    eprintln!();
    eprintln!("=== Configuration Summary ===");
    eprintln!("LLVM version : {}", tools.llvm_version);
    eprintln!("llvm-config  : {}", tools.llvm_config.display());
    eprintln!("clang        : {}", tools.clang.display());
    eprintln!("clang++      : {}", tools.clangxx.display());
    eprintln!("llvm-ar      : {}", tools.llvm_ar.display());
    eprintln!("llvm-link    : {}", tools.llvm_link.display());
    match &tools.llvm_objcopy {
        Some(llvm_objcopy) => eprintln!("llvm-objcopy : {}", llvm_objcopy.display()),
        None => eprintln!("llvm-objcopy : (not found, optional)"),
    }

    if args.dry_run {
        eprintln!();
        eprintln!("=== Generated config.toml (dry run) ===");
        print!("{}", toml_content);
        return Ok(());
    }

    // Resolved through the same helper the loader uses, so init cannot write a
    // configuration that the wrappers will not read. An explicit -o still wins.
    let output_path = args
        .output
        .as_deref()
        .map_or_else(config_filepath, expand_tilde);

    // Create parent directory if needed
    if let Some(parent) = output_path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(|err| {
            Error::ConfigError(format!(
                "Failed to create config directory {:?}: {}",
                parent, err
            ))
        })?;
    }

    fs::write(&output_path, &toml_content).map_err(|err| {
        Error::ConfigError(format!(
            "Failed to write config to {:?}: {}",
            output_path, err
        ))
    })?;

    eprintln!();
    eprintln!("Config written to: {}", output_path.display());
    eprintln!();
    eprintln!("You can now use rllvm-cc, rllvm-cxx, and rllvm-get-bc.");
    eprintln!("To customize, edit: {}", output_path.display());

    Ok(())
}
