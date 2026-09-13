//! Current-tree compilation database import and selective bitcode generation.

use crate::{
    arg_parser::CompilerArgsInfo,
    catalog::{
        CatalogOrigin, CompilationRecord, CompilerIdentity, ModuleCatalog, ModuleRecord,
        ModuleStatus, SourceAssociation, hash_bytes, hash_file, identity, write_catalog,
    },
    error::Error,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

/// Imported entries retain database occurrences, including unsupported commands.
pub struct CompilationDatabase {
    path: PathBuf,
    digest: String,
    entries: Vec<ModuleRecord>,
}

/// Explicit selection and analysis settings for a new generation run.
pub struct GenerateOptions {
    pub output_dir: PathBuf,
    pub sources: Vec<PathBuf>,
    pub entries: Vec<String>,
    pub extra_arguments: Vec<String>,
    pub jobs: usize,
}

#[derive(Deserialize)]
struct DatabaseEntry {
    directory: PathBuf,
    file: PathBuf,
    arguments: Option<Vec<String>>,
    command: Option<String>,
    output: Option<PathBuf>,
}

impl CompilationDatabase {
    /// Read a JSON database, or compile_commands.json within a build directory.
    /// Source files and compilers need not exist for listing.
    pub fn load(input: &Path) -> Result<Self, Error> {
        let input = if input.is_dir() {
            input.join("compile_commands.json")
        } else {
            input.to_path_buf()
        };
        let path = resolve(&input, &std::env::current_dir()?);
        let bytes = fs::read(&path)?;
        let raw: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("invalid compilation database: {error}")))?;
        let digest = hash_bytes(&bytes);
        let directory = path
            .parent()
            .ok_or_else(|| invalid("database has no parent directory"))?;
        let entries = raw
            .into_iter()
            .enumerate()
            .map(|(index, raw)| {
                let mut module = ModuleRecord::new(identity(&[
                    "compilation-entry-v1",
                    &digest,
                    &index.to_string(),
                ]));
                module.unavailable_metadata = vec![
                    "historical_source_and_dependencies".into(),
                    "original_build_environment".into(),
                    "executable_membership".into(),
                ];
                let entry = serde_json::from_value::<DatabaseEntry>(raw)
                    .map_err(|error| invalid(format!("malformed entry {index}: {error}")));
                let result =
                    entry.and_then(|entry| import_entry(&mut module, entry, index, directory));
                if let Err(error) = result {
                    module.status = ModuleStatus::Unsupported;
                    module.diagnostics.push(error.to_string());
                }
                module
            })
            .collect();
        Ok(Self {
            path,
            digest,
            entries,
        })
    }

    /// List compilations and unsupported-entry diagnostics without compiling.
    pub fn list(&self) -> ModuleCatalog {
        self.catalog(self.entries.clone())
    }

    /// Generate selected compilations, retaining per-entry failures in the catalog.
    /// A caller should return nonzero if any record is not available.
    pub fn generate(&self, options: &GenerateOptions) -> Result<ModuleCatalog, Error> {
        let directory = std::env::current_dir()?;
        let selected = self.select(&options.sources, &options.entries, &directory)?;
        if options.jobs == 0 {
            return Err(invalid("--jobs must be at least one"));
        }
        let output = resolve(&options.output_dir, &directory);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&output).map_err(|error| {
            invalid(format!(
                "output directory must be new ({}): {error}",
                output.display()
            ))
        })?;
        fs::create_dir(output.join("modules"))?;
        fs::create_dir(output.join("diagnostics"))?;
        let environment = analysis_environment();
        let mut compilers = BTreeMap::new();
        let mut prepared = Vec::new();
        for index in selected {
            let module = self.entries[index].clone();
            let compiler = module
                .compilation
                .as_ref()
                .filter(|_| module.status == ModuleStatus::Planned)
                .map(|compilation| {
                    let path = which::which_in(
                        &compilation.recorded_arguments[0],
                        std::env::var_os("PATH"),
                        &compilation.directory,
                    )
                    .map_err(|error| format!("cannot resolve recorded compiler: {error}"))?;
                    compilers
                        .entry(path.clone())
                        .or_insert_with(|| {
                            compiler_context(&path, &compilation.directory)
                                .map_err(|error| error.to_string())
                        })
                        .clone()
                });
            prepared.push((module, compiler));
        }
        let modules = bounded_map(&prepared, options.jobs, |_, (original, compiler)| {
            generate_entry(
                original.clone(),
                compiler.as_ref(),
                options,
                &output,
                &environment,
            )
        })?;
        let mut catalog = self.catalog(modules);
        catalog.scope.analysis_arguments = options.extra_arguments.clone();
        catalog.scope.selection.module_ids = options.entries.clone();
        catalog.scope.selection.sources = options
            .sources
            .iter()
            .map(|source| resolve(source, &directory))
            .collect();
        write_catalog(&output.join("catalog.json"), &catalog)?;
        Ok(catalog)
    }

    fn catalog(&self, modules: Vec<ModuleRecord>) -> ModuleCatalog {
        let mut catalog = ModuleCatalog::new(
            CatalogOrigin {
                kind: "compilation_database".into(),
                input: self.path.clone(),
                sha256: Some(self.digest.clone()),
            },
            "selected_current_tree_compilations",
            modules,
        );
        catalog.scope.total_entries = self.entries.len();
        catalog.scope.limitations.extend([
            "The database describes compilations, not executable membership or dependency completeness.".into(),
            "Sources and generated headers come from the current tree; historical inputs and original build environment are unknown.".into(),
            "Only selected compilation environment variables are recorded; the analysis environment is not a complete snapshot.".into(),
        ]);
        catalog
    }

    /// Select exact source paths and entry IDs: OR within a kind, AND across kinds.
    pub fn select(
        &self,
        sources: &[PathBuf],
        entries: &[String],
        directory: &Path,
    ) -> Result<Vec<usize>, Error> {
        let sources: Vec<_> = sources
            .iter()
            .map(|source| resolve(source, directory))
            .collect();
        for source in &sources {
            if !self.entries.iter().any(|entry| {
                entry
                    .sources
                    .iter()
                    .any(|association| &association.path == source)
            }) {
                return Err(invalid(format!(
                    "unmatched source selector: {}",
                    source.display()
                )));
            }
        }
        for id in entries {
            if !self.entries.iter().any(|entry| &entry.id == id) {
                return Err(invalid(format!("unmatched entry selector: {id}")));
            }
        }
        let selected: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                (entries.is_empty() || entries.contains(&entry.id))
                    && (sources.is_empty()
                        || entry
                            .sources
                            .iter()
                            .any(|source| sources.contains(&source.path)))
            })
            .map(|(index, _)| index)
            .collect();
        if selected.is_empty() {
            return Err(invalid("selection contains no compilation entries"));
        }
        Ok(selected)
    }
}

#[derive(Clone)]
struct CompilerContext {
    identity: CompilerIdentity,
    llvm_dis: PathBuf,
}

fn compiler_context(path: &Path, directory: &Path) -> Result<CompilerContext, Error> {
    let output = crate::utils::execute_llvm_tool_in_for_output(path, &["--version"], directory)?;
    if !output.status.success() {
        return Err(Error::ExecutionFailure(format!(
            "compiler version query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !version.to_ascii_lowercase().contains("clang") {
        return Err(invalid("recorded driver does not identify itself as Clang"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("compiler has no parent directory"))?;
    let llvm_dis = if parent.join("llvm-dis").is_file() {
        parent.join("llvm-dis")
    } else {
        let config = crate::utils::find_llvm_config()?;
        let output =
            crate::utils::execute_llvm_tool_in_for_output(config, &["--bindir"], directory)?;
        if !output.status.success() {
            return Err(Error::ExecutionFailure(
                "llvm-config --bindir failed".into(),
            ));
        }
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("llvm-dis")
    };
    Ok(CompilerContext {
        identity: CompilerIdentity {
            path: path.to_path_buf(),
            realpath: fs::canonicalize(path).ok(),
            version,
            sha256: Some(hash_file(path)?),
        },
        llvm_dis,
    })
}

fn analysis_environment() -> BTreeMap<String, String> {
    const NAMES: &[&str] = &[
        "PATH",
        "CPATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "OBJC_INCLUDE_PATH",
        "SDKROOT",
        "MACOSX_DEPLOYMENT_TARGET",
        "IPHONEOS_DEPLOYMENT_TARGET",
        "TVOS_DEPLOYMENT_TARGET",
        "WATCHOS_DEPLOYMENT_TARGET",
        "VISIONOS_DEPLOYMENT_TARGET",
        "LIBRARY_PATH",
        "COMPILER_PATH",
        "GCC_EXEC_PREFIX",
        "SOURCE_DATE_EPOCH",
    ];
    NAMES
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).into(), value))
        })
        .collect()
}

fn generate_entry(
    mut module: ModuleRecord,
    compiler: Option<&Result<CompilerContext, String>>,
    options: &GenerateOptions,
    output: &Path,
    environment: &BTreeMap<String, String>,
) -> ModuleRecord {
    let diagnostic = PathBuf::from("diagnostics").join(format!("{}.txt", module.id));
    module.diagnostic_path = Some(diagnostic.clone());
    if let Some(compilation) = &mut module.compilation {
        compilation.extra_arguments = options.extra_arguments.clone();
        compilation.environment = environment.clone();
    }
    let mut diagnostics = Vec::new();
    let result = materialize_entry(
        &mut module,
        compiler,
        options,
        output,
        environment,
        &mut diagnostics,
    );
    if let Err(error) = result {
        if module.status != ModuleStatus::Unsupported {
            module.status = ModuleStatus::Failed;
        }
        module.diagnostics.push(error.to_string());
    }
    diagnostics.extend_from_slice(module.diagnostics.join("\n").as_bytes());
    if let Err(error) = fs::write(output.join(&diagnostic), diagnostics) {
        module.status = ModuleStatus::Failed;
        module.diagnostic_path = None;
        module
            .diagnostics
            .push(format!("cannot write diagnostics: {error}"));
    }
    module
}

fn materialize_entry(
    module: &mut ModuleRecord,
    compiler: Option<&Result<CompilerContext, String>>,
    options: &GenerateOptions,
    output: &Path,
    environment: &BTreeMap<String, String>,
    diagnostics: &mut Vec<u8>,
) -> Result<(), Error> {
    if module.status == ModuleStatus::Unsupported {
        return Ok(());
    }
    let compiler = compiler
        .ok_or_else(|| invalid("compiler identity unavailable"))?
        .as_ref()
        .map_err(|message| Error::ExecutionFailure(message.clone()))?;
    module.compiler = Some(compiler.identity.clone());
    let compilation = module
        .compilation
        .as_mut()
        .ok_or_else(|| invalid("compilation record unavailable"))?;
    compilation.extra_arguments = options.extra_arguments.clone();
    compilation.environment = environment.clone();
    for name in [
        "DEPENDENCIES_OUTPUT",
        "SUNPRO_DEPENDENCIES",
        "CCC_OVERRIDE_OPTIONS",
        "CCC_ADD_ARGS",
        "CLANG_CONFIG_FILE_SYSTEM_DIR",
        "CLANG_CONFIG_FILE_USER_DIR",
    ] {
        if std::env::var_os(name).is_some() {
            module.status = ModuleStatus::Unsupported;
            return Err(invalid(format!(
                "environment variable {name} may introduce unowned outputs or hidden driver arguments"
            )));
        }
    }
    let mut arguments = compilation.recorded_arguments[1..].to_vec();
    arguments.extend(options.extra_arguments.iter().cloned());
    let parsed = classify_arguments(&arguments, &compilation.directory)
        .inspect_err(|_| module.status = ModuleStatus::Unsupported)?;
    let identity_options = serde_json::to_string(&(
        &options.extra_arguments,
        parsed.compile_args(),
        environment,
        &compiler.identity,
    ))
    .map_err(|error| invalid(error.to_string()))?;
    compilation.analysis_id = identity(&[
        "compilation-analysis-v1",
        module.configuration_id.as_deref().unwrap_or_default(),
        &identity_options,
    ]);
    let source = module
        .sources
        .first_mut()
        .ok_or_else(|| invalid("source association unavailable"))?;
    if resolve(Path::new(&parsed.input_files()[0]), &compilation.directory) != source.path {
        module.status = ModuleStatus::Unsupported;
        return Err(invalid("analysis overrides changed source input"));
    }
    source.content_sha256 = hash_file(&source.path).ok();
    let temporary = tempfile::NamedTempFile::new_in(output)?;
    let mut arguments = crate::materialize::bitcode_arguments(
        parsed.compile_args(),
        &[],
        Path::new(&parsed.input_files()[0]),
        temporary.path(),
        None,
    )?;
    // Driver crash reports are auxiliary artifacts, not analysis settings.
    arguments.insert(0, "-fno-crash-diagnostics".into());
    compilation.effective_arguments =
        std::iter::once(compiler.identity.path.to_string_lossy().into_owned())
            .chain(arguments.iter().cloned())
            .collect();
    let result = crate::utils::execute_llvm_tool_in_for_output(
        &compiler.identity.path,
        &arguments,
        &compilation.directory,
    )?;
    diagnostics.extend_from_slice(&result.stderr);
    diagnostics.extend_from_slice(&result.stdout);
    if !result.status.success() {
        return Err(Error::ExecutionFailure(format!(
            "compiler exited with {}",
            result.status
        )));
    }
    let inspected =
        crate::catalog::inspect_bitcode(temporary.path(), &compiler.llvm_dis, module.id.clone());
    module.target_triple = inspected.target_triple;
    module.data_layout = inspected.data_layout;
    module.debug_info = inspected.debug_info;
    module.content_sha256 = inspected.content_sha256;
    module.status = inspected.status;
    module.diagnostics.extend(inspected.diagnostics);
    module.ir_stage = Some("translation_unit_before_link".into());
    if source.content_sha256 != hash_file(&source.path).ok() {
        source.content_sha256 = None;
        module
            .diagnostics
            .push("source changed during compilation; current source hash unavailable".into());
    }
    let relative = PathBuf::from("modules").join(format!("{}.bc", module.id));
    fs::File::open(temporary.path())?.sync_all()?;
    temporary
        .persist_noclobber(output.join(&relative))
        .map_err(|error| Error::Io(error.error))?;
    module.path = Some(relative);
    Ok(())
}

fn import_entry(
    module: &mut ModuleRecord,
    entry: DatabaseEntry,
    index: usize,
    database_directory: &Path,
) -> Result<(), Error> {
    let directory = resolve(&entry.directory, database_directory);
    let source = resolve(&entry.file, &directory);
    module.sources.push(SourceAssociation {
        path: source.clone(),
        directory: Some(directory.clone()),
        origin: "compilation_database".into(),
        content_sha256: None,
    });
    let arguments = match entry.arguments {
        Some(arguments) => arguments,
        None => decode_command(
            entry
                .command
                .as_deref()
                .ok_or_else(|| invalid("entry requires arguments or command"))?,
        )?,
    };
    let compiler = arguments
        .first()
        .ok_or_else(|| invalid("empty compiler arguments"))?;
    let mut recorded_output = entry.output.map(|output| resolve(&output, &directory));
    let parsed = classify_arguments(&arguments[1..], &directory);
    if let Ok(parsed) = &parsed
        && recorded_output.is_none()
        && !parsed.output_filename().is_empty()
    {
        recorded_output = Some(resolve(Path::new(parsed.output_filename()), &directory));
    }
    let identity_arguments = parsed
        .as_ref()
        .map(|parsed| parsed.expanded_args())
        .unwrap_or(&arguments[1..]);
    let command_identity = serde_json::to_string(&(compiler, identity_arguments, &recorded_output))
        .map_err(|error| invalid(error.to_string()))?;
    module.configuration_id = Some(identity(&[
        "compilation-configuration-v1",
        &directory.to_string_lossy(),
        &source.to_string_lossy(),
        &command_identity,
    ]));
    module.compilation = Some(CompilationRecord {
        entry_index: index,
        directory: directory.clone(),
        recorded_arguments: arguments.clone(),
        recorded_output,
        effective_arguments: Vec::new(),
        analysis_id: String::new(),
        extra_arguments: Vec::new(),
        environment: BTreeMap::new(),
        environment_complete: false,
    });
    validate_driver(compiler)?;
    let parsed = parsed?;
    if resolve(Path::new(&parsed.input_files()[0]), &directory) != source {
        return Err(invalid(
            "entry file does not match its single compiler input",
        ));
    }
    Ok(())
}

fn validate_driver(compiler: &str) -> Result<(), Error> {
    let name = Path::new(compiler)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let supported = ["clang", "clang++"].iter().any(|driver| {
        name == *driver
            || name
                .strip_prefix(&format!("{driver}-"))
                .is_some_and(|version| {
                    !version.is_empty()
                        && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                })
    });
    if !supported {
        return Err(invalid(format!(
            "unsupported compiler or launcher {compiler:?}; direct clang/clang++ entries are required"
        )));
    }
    Ok(())
}

// Lexical resolution keeps list identities independent of source existence.
fn resolve(path: &Path, directory: &Path) -> PathBuf {
    let joined = directory.join(path);
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

fn bounded_map<T: Sync, R: Send>(
    entries: &[T],
    jobs: usize,
    operation: impl Fn(usize, &T) -> R + Sync,
) -> Result<Vec<R>, Error> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    if jobs == 0 {
        return Err(invalid("--jobs must be at least one"));
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut failure = None;
        for _ in 0..jobs.min(entries.len()) {
            let next = &next;
            let operation = &operation;
            match std::thread::Builder::new().spawn_scoped(scope, move || {
                let mut results = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = entries.get(index) else {
                        break;
                    };
                    results.push((index, operation(index, entry)));
                }
                results
            }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    failure = Some(Error::Io(error));
                    break;
                }
            }
        }
        let mut results = Vec::with_capacity(entries.len());
        for handle in handles {
            match handle.join() {
                Ok(values) => results.extend(values),
                Err(_) => {
                    failure = Some(Error::ExecutionFailure(
                        "compilation worker panicked".into(),
                    ))
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, value)| value).collect())
    })
}

fn classify_arguments(args: &[String], directory: &Path) -> Result<CompilerArgsInfo, Error> {
    let mut parsed = CompilerArgsInfo::default();
    parsed.parse_args_in(args, directory)?;
    check_owned_outputs(parsed.expanded_args())?;
    if !parsed.is_compile_only()
        || parsed.input_files().len() != 1
        || !parsed.object_files().is_empty()
    {
        return Err(invalid(
            "expected a single-source compile-only Clang command (-c)",
        ));
    }
    if parsed.is_assembly()
        || parsed.is_assemble_only()
        || parsed.is_preprocess_only()
        || parsed.is_dependency_only()
        || parsed.is_print_only()
    {
        return Err(invalid(
            "assembly, preprocessing, dependency-only and query commands are unsupported",
        ));
    }
    if !parsed.forbidden_flags().is_empty() {
        return Err(invalid(format!(
            "unsupported flags: {}",
            parsed.forbidden_flags().join(" ")
        )));
    }
    if crate::arg_parser::universal_build_architectures(parsed.compile_args()).len() > 1 {
        return Err(invalid("universal builds are unsupported"));
    }
    // An assembly or header language can also be selected with -x.
    for pair in parsed.compile_args().windows(2) {
        if pair[0] == "-x"
            && !matches!(
                pair[1].as_str(),
                "c" | "c++" | "objective-c" | "objective-c++" | "none"
            )
        {
            return Err(invalid(format!("unsupported input language: {}", pair[1])));
        }
    }
    Ok(parsed)
}

fn check_owned_outputs(args: &[String]) -> Result<(), Error> {
    // Secondary invocations cannot inherit hidden outputs or driver escapes.
    // Reject these explicitly rather than attempt to rewrite opaque cc1/config
    // arguments. Native -o and -M* flags are stripped by the shared parser.
    const UNSUPPORTED: &[&str] = &[
        "-save-temps",
        "--save-temps",
        "-gsplit-dwarf",
        "-fprofile-generate",
        "-fprofile-instr-generate",
        "-fcs-profile-generate",
        "-fprofile-arcs",
        "-ftest-coverage",
        "--coverage",
        "-fcoverage",
        "-ftime-trace",
        "-fsave-optimization-record",
        "-foptimization-record",
        "-serialize-diagnostics",
        "--serialize-diagnostics",
        "-fmodules",
        "-fcxx-modules",
        "-fimplicit-modules",
        "-fmodule-output",
        "-fmodule-header",
        "-fmodule-name",
        "-fmodule-map",
        "-fmodule-cache",
        "-index-",
        "-gen-",
        "-dump",
        "-aux-info",
        "-dependency-",
        "-working-directory",
        "--config",
        "-config",
        "-Xclang",
        "-Xpreprocessor",
        "-Xarch_",
        "-mllvm",
        "-Wp,",
        "-cc1",
        "--analyze",
        "-fsyntax-only",
        "-fdriver-only",
        "-###",
        "--help",
        "-help",
        "-fplugin",
        "-load",
        "-add-plugin",
        "-extract-api",
        "-emit-interface-stubs",
        "-fcrash-diagnostics-dir",
        "-fstack-usage",
        "-fcallgraph-info",
        "-fproc-stat-report",
        "-fdiagnostics-serialization-file",
    ];
    for arg in args {
        if arg == "--"
            || arg.contains('\0')
            || UNSUPPORTED.iter().any(|prefix| arg.starts_with(prefix))
        {
            return Err(invalid(format!("unsupported option or side output: {arg}")));
        }
    }
    Ok(())
}

// This is shell *decoding*, distinct from LLVM's GNU response tokenizer:
// preserve empty arguments, reject expansions/operators, and never run a shell.
fn decode_command(command: &str) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\0' {
            return Err(invalid("NUL in command"));
        }
        match ch {
            ch if quote == Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    token.push(ch);
                }
            }
            '\\' => {
                let next = chars
                    .next()
                    .ok_or_else(|| invalid("trailing command escape"))?;
                if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                    token.push('\\');
                }
                if next != '\n' {
                    token.push(next);
                    started = true;
                }
            }
            '\'' | '"' if quote.is_none() => {
                quote = Some(ch);
                started = true;
            }
            '"' if quote == Some('"') => quote = None,
            '$' | '`' => return Err(invalid("shell expansions are unsupported; use arguments")),
            ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | ']' | '~'
                if quote.is_none() =>
            {
                return Err(invalid(
                    "shell operators and expansions are unsupported; use arguments",
                ));
            }
            '\n' | '\r' if quote.is_none() => {
                return Err(invalid("shell command separators are unsupported"));
            }
            ' ' | '\t' if quote.is_none() => {
                if started {
                    result.push(std::mem::take(&mut token));
                    started = false;
                }
            }
            _ => {
                token.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(invalid("unterminated command quote"));
    }
    if started {
        result.push(token);
    }
    if result.is_empty() {
        return Err(invalid("empty compiler command"));
    }
    Ok(result)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidArguments(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(contents: &str) -> (tempfile::TempDir, CompilationDatabase) {
        let scratch = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join("compile_commands.json"), contents).unwrap();
        let database = CompilationDatabase::load(scratch.path()).unwrap();
        (scratch, database)
    }

    #[test]
    fn database_preserves_occurrences_configurations_and_argument_precedence() {
        let (scratch, database) = database(
            r#"[
            {"directory":"build/../build", "file":"../src/a.c", "arguments":["clang","-O2","-c","../src/a.c","-o","a.o"], "command":"false"},
            {"directory":"build", "file":"../src/a.c", "arguments":["clang","-O2","-c","../src/a.c","-o","a.o"]},
            {"directory":"build", "file":"../src/a.c", "command":"clang -O0 -c ../src/a.c -o other.o"}
        ]"#,
        );
        let catalog = database.list();
        assert_eq!(catalog.modules.len(), 3);
        let a = &catalog.modules[0];
        let b = &catalog.modules[1];
        let c = &catalog.modules[2];
        assert_ne!(a.id, b.id);
        assert_eq!(a.configuration_id, b.configuration_id);
        assert_ne!(a.configuration_id, c.configuration_id);
        assert_eq!(a.status, crate::catalog::ModuleStatus::Planned);
        assert_eq!(a.sources[0].path, scratch.path().join("src/a.c"));
        assert_eq!(
            a.compilation.as_ref().unwrap().recorded_arguments[0],
            "clang"
        );
        assert_eq!(
            a.compilation.as_ref().unwrap().directory,
            scratch.path().join("build")
        );
    }

    #[test]
    fn selection_unions_values_intersects_kinds_and_rejects_unmatched_filters() {
        let (scratch, database) = database(
            r#"[
            {"directory":".","file":"a.c","arguments":["clang","-c","a.c"]},
            {"directory":".","file":"a.c","arguments":["clang","-O2","-c","a.c"]},
            {"directory":".","file":"b.c","arguments":["clang","-c","b.c"]}
        ]"#,
        );
        let catalog = database.list();
        assert_eq!(
            database
                .select(&["a.c".into()], &[], scratch.path())
                .unwrap(),
            vec![0, 1]
        );
        assert_eq!(
            database
                .select(
                    &["a.c".into(), "b.c".into()],
                    &[catalog.modules[1].id.clone(), catalog.modules[2].id.clone()],
                    scratch.path()
                )
                .unwrap(),
            vec![1, 2]
        );
        assert!(
            database
                .select(&["missing.c".into()], &[], scratch.path())
                .is_err()
        );
        assert!(
            database
                .select(&[], &["missing".into()], scratch.path())
                .is_err()
        );
        assert!(
            database
                .select(
                    &["a.c".into()],
                    &[catalog.modules[2].id.clone()],
                    scratch.path()
                )
                .is_err()
        );
    }

    #[test]
    fn list_retains_malformed_and_unsupported_entries_without_compiling() {
        let (_, database) = database(
            r#"[
            {"directory":"missing", "file":"a.c", "arguments":["ccache","clang","-c","a.c"]},
            {"directory":"missing", "file":"a.c", "arguments":["gcc","-c","a.c"]},
            {"directory":".", "file":"a.c", "arguments":[]},
            {"directory":".", "file":"a.c"},
            {"directory":".", "file":"a.c", "arguments":"clang -c a.c"}
        ]"#,
        );
        assert!(database.list().modules.iter().all(|module| module.status
            == crate::catalog::ModuleStatus::Unsupported
            && !module.diagnostics.is_empty()));
    }

    #[test]
    fn workers_are_bounded_and_results_keep_database_order() {
        use std::sync::{
            Barrier, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let barrier = Barrier::new(3);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let threads = Mutex::new(std::collections::HashSet::new());
        let result = bounded_map(&(0..9).collect::<Vec<_>>(), 3, |index, entry| {
            threads.lock().unwrap().insert(std::thread::current().id());
            let running = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(running, Ordering::SeqCst);
            barrier.wait();
            active.fetch_sub(1, Ordering::SeqCst);
            barrier.wait();
            assert_eq!(index, *entry);
            entry * 2
        })
        .unwrap();
        assert_eq!(result, (0..9).map(|value| value * 2).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert_eq!(threads.lock().unwrap().len(), 3);
        assert!(bounded_map(&[1], 0, |_, value| *value).is_err());
    }

    #[test]
    fn imported_arguments_preserve_semantics_and_strip_owned_outputs() {
        let args = [
            "-O2",
            "-pthread",
            "-D",
            "VALUE=2",
            "-I",
            "include",
            "-c",
            "source.c",
            "-ooriginal.o",
            "-MMD",
            "-MForiginal.d",
        ]
        .map(String::from);
        let parsed = classify_arguments(&args, Path::new(".")).unwrap();
        let generated = crate::materialize::bitcode_arguments(
            parsed.compile_args(),
            &[],
            Path::new("source.c"),
            Path::new("module.bc"),
            None,
        )
        .unwrap();
        assert_eq!(
            generated,
            [
                "-O2",
                "-pthread",
                "-D",
                "VALUE=2",
                "-I",
                "include",
                "-emit-llvm",
                "-c",
                "-o",
                "module.bc",
                "source.c"
            ]
        );
    }

    #[test]
    fn importer_rejects_modes_and_side_outputs_it_cannot_own() {
        for flag in [
            "-save-temps",
            "--save-temps=obj",
            "-gsplit-dwarf",
            "-fprofile-arcs",
            "--coverage",
            "-ftime-trace=original.json",
            "-fsave-optimization-record",
            "-fmodules",
            "-Xclang",
            "-Wp,-MD,original.d",
            "--config=local.cfg",
            "-fsyntax-only",
            "-S",
            "--analyze",
            "-E",
        ] {
            let args = ["-c", "source.c", flag].map(String::from);
            assert!(
                classify_arguments(&args, Path::new(".")).is_err(),
                "accepted {flag}"
            );
        }
        for args in [
            vec!["input.o"],
            vec!["-c", "one.c", "two.c"],
            vec!["-c", "input.s"],
            vec!["-c", "source.c", "-o"],
        ] {
            assert!(
                classify_arguments(
                    &args.into_iter().map(String::from).collect::<Vec<_>>(),
                    Path::new(".")
                )
                .is_err()
            );
        }
    }

    #[test]
    fn command_decoding_preserves_quoted_empty_arguments_and_escapes() {
        assert_eq!(
            decode_command(r#"clang -I "" '-DNAME=a b' "-DQUOTE=\"hi\"" -c file\ name.c"#).unwrap(),
            [
                "clang",
                "-I",
                "",
                "-DNAME=a b",
                "-DQUOTE=\"hi\"",
                "-c",
                "file name.c"
            ]
        );
        assert_eq!(
            decode_command(r#"clang '-DVALUE=$HOME' "-DWIN=C:\src""#).unwrap(),
            ["clang", "-DVALUE=$HOME", r"-DWIN=C:\src"]
        );
    }

    #[test]
    fn command_decoding_rejects_shell_operations_and_malformed_quotes() {
        for command in [
            "clang a.c | cat",
            "clang a.c && touch file",
            "clang a.c; touch file",
            "clang $(echo a.c)",
            "clang `echo a.c`",
            "clang $SOURCE",
            "clang a.c > out",
            "clang 'a.c",
            "clang a.c \\",
        ] {
            assert!(decode_command(command).is_err(), "accepted {command:?}");
        }
    }
}
