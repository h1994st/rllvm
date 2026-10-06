use std::{
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{CommandFactory, Parser};
use rllvm_core::{
    config::try_rllvm_config,
    error::Error,
    utils::{print_stdout, split_response_arguments},
};
use rllvm_query::{
    Color, FactsCache, Overlay, OverlaySummary, Query, Record, Session, TextMode,
    cli::{CacheAction, ClosureDirection, OverlayAction, QueryArgs, QueryCommand, QueryModifiers},
    default_overlay_path,
    index::Direction,
    llvm_version, mcp, open_with_cache,
    overlay::verdict_label,
    run_with_overlay,
};
use tracing_subscriber::FmtSubscriber;

/// Converts a parsed subcommand into the [`Query`] it names, or `None` for
/// the four variants that name a mode rather than a query: `Mcp` (serve
/// over MCP stdio), `Completions` (print a completion script), `Cache`
/// (inspect or prune the facts cache) and `Overlay` (keep agent-authored
/// edges), all of which this binary has already handled by the time a query
/// would run.
///
/// Exhaustive over `QueryCommand`, so a new `QueryCommand` variant with no
/// arm here fails to compile. That alone does not catch the opposite drift --
/// a new [`Query`] variant added without a matching `QueryCommand` --
/// which compiles cleanly on its own. `cli_command_for` in this file's tests
/// closes that gap: it is exhaustive over `Query`, so a new `Query` variant
/// fails to compile there instead, until this file is updated to drive it
/// from the command line.
fn to_query(command: QueryCommand, modifiers: QueryModifiers) -> Option<Query> {
    let QueryModifiers {
        heuristics,
        include_overlay,
        min_confidence,
    } = modifiers;
    Some(match command {
        QueryCommand::Defs { name } => Query::Defs { name },
        QueryCommand::At { file, line } => Query::At { file, line },
        QueryCommand::Callers { name } => Query::Callers { name },
        QueryCommand::Callees { name } => Query::Callees { name },
        QueryCommand::Uses { name } => Query::Uses { name },
        QueryCommand::Reach { from, to } => Query::Reach {
            from,
            to,
            include_overlay,
            min_confidence,
        },
        QueryCommand::Closure { name, direction } => Query::Closure {
            name,
            direction: match direction {
                ClosureDirection::In => Direction::In,
                ClosureDirection::Out => Direction::Out,
            },
            include_overlay,
            min_confidence,
        },
        QueryCommand::Externals => Query::Externals,
        QueryCommand::FfiExports => Query::FfiExports,
        QueryCommand::IndirectTargets { at } => Query::IndirectTargets { at, heuristics },
        QueryCommand::ResolutionCandidates => Query::ResolutionCandidates,
        QueryCommand::Mcp => return None,
        // `main` answers this one before `run_query` is ever called; the arm
        // is here so adding a mode variant cannot compile without a decision.
        QueryCommand::Completions { .. } => return None,
        QueryCommand::Cache { .. } => return None,
        QueryCommand::Overlay { .. } => return None,
    })
}

/// One line of stdin: a query subcommand, written as it would follow
/// `rllvm-query --catalog <catalog>`. Only the [`QueryModifiers`] flags may
/// accompany it; the output format, the catalog and the overlay file belong
/// to the whole run.
#[derive(Parser)]
#[command(name = "rllvm-query", no_binary_name = true)]
struct QueryLine {
    #[command(flatten)]
    modifiers: QueryModifiers,

    #[command(subcommand)]
    command: QueryCommand,
}

/// Parses and validates every query on stdin, before any catalog is read,
/// so a typo on the last line costs a diagnostic rather than a full load.
/// Each query is returned with its line as written, which heads its answer.
fn parse_queries(input: &str, modifiers: QueryModifiers) -> Result<Vec<(String, Query)>, Error> {
    let mut queries = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // One line per diagnostic: clap's message runs to a usage block,
        // which here would describe a command line the reader never typed.
        let at_line = |reason: String| {
            let reason = reason.split("\n\n").next().unwrap_or_default();
            let reason = reason.strip_prefix("error: ").unwrap_or(reason);
            let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
            Error::InvalidArguments(format!("line {}: {reason}", index + 1))
        };
        let arguments = split_response_arguments(line);
        let parsed =
            QueryLine::try_parse_from(&arguments).map_err(|error| at_line(error.to_string()))?;
        let query = to_query(parsed.command, modifiers.with(parsed.modifiers))
            .ok_or_else(|| at_line(format!("`{}` names a mode, not a query", arguments[0])))?;
        query.validate().map_err(|error| match error {
            Error::InvalidArguments(reason) => at_line(reason),
            other => at_line(other.to_string()),
        })?;
        queries.push((line.to_string(), query));
    }
    Ok(queries)
}

/// The facts cache the configuration asks for, or `None` when disabled.
fn facts_cache() -> Result<Option<FactsCache>, Error> {
    Ok(FactsCache::from_config(try_rllvm_config()?))
}

/// `rllvm-query cache [clear [--stale]]`: needs no catalog.
fn run_cache(action: Option<&CacheAction>, format: Format) -> Result<(), Error> {
    let config = try_rllvm_config()?;
    let cache = FactsCache::configured(config)?;
    match action {
        Some(CacheAction::Clear { stale }) => {
            let cleared = cache.clear(*stale);
            match format {
                Format::Json => print_stdout(&format!(
                    "{}\n",
                    serde_json::to_string(&cleared)
                        .map_err(|error| Error::InvalidArguments(error.to_string()))?
                )),
                Format::Text(_) => {
                    let orphans = if cleared.orphans > 0 {
                        format!(" and {} orphaned temp file(s)", cleared.orphans)
                    } else {
                        String::new()
                    };
                    print_stdout(&format!(
                        "removed {} entries{orphans}, {}\n",
                        cleared.entries,
                        rllvm_query::render::human_bytes(cleared.bytes)
                    ))
                }
            }
        }
        None => {
            let usage = cache.usage(config.query_cache_enabled());
            match format {
                Format::Json => print_stdout(&format!(
                    "{}\n",
                    serde_json::to_string_pretty(&usage)
                        .map_err(|error| Error::InvalidArguments(error.to_string()))?
                )),
                Format::Text(_) => print_stdout(&rllvm_query::render_usage(&usage)),
            }
        }
    }
}

/// Parses the overlay records piped on stdin, one JSON object per line,
/// each kept with its 1-based line so a record that fails validation later
/// is named by its line too. Blank lines are skipped.
fn parse_records(input: &str) -> Result<Vec<(usize, Record)>, Error> {
    let mut records = Vec::new();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = Record::parse_labeled(line, format_args!("line {}", index + 1))?;
        records.push((index + 1, record));
    }
    Ok(records)
}

/// The overlay file: `--overlay`, or the default beside the catalog.
fn overlay_path(catalog: &Path, overlay: Option<&Path>) -> PathBuf {
    overlay.map_or_else(|| default_overlay_path(catalog), Path::to_path_buf)
}

/// The overlay `query` walks, opened against `session` when it asks for one
/// and `opened` holds none yet: at most once per run. A file that cannot be
/// read is an error, never a silent direct-only answer. A missing file is an
/// empty overlay at the default path, where nothing may be recorded yet, but
/// an error when `--overlay` named it: that is a mistyped path.
fn overlay_for<'o>(
    opened: &'o mut Option<Overlay>,
    session: &Session,
    catalog: &Path,
    named: Option<&Path>,
    query: &Query,
) -> Result<Option<&'o Overlay>, Error> {
    if query.walks_overlay() && opened.is_none() {
        let path = overlay_path(catalog, named);
        if named.is_some()
            && let Err(error) = std::fs::metadata(&path)
            && error.kind() == std::io::ErrorKind::NotFound
        {
            return Err(Error::InvalidArguments(format!(
                "{}: no such overlay; --overlay names an existing file for a walk to read",
                path.display()
            )));
        }
        *opened = Some(Overlay::open(session, &path)?);
    }
    Ok(opened.as_ref())
}

/// `rllvm-query --catalog <catalog> overlay record|list|compact`. The
/// overlay file defaults to one beside the catalog.
fn run_overlay(
    action: &OverlayAction,
    catalog: Option<&Path>,
    overlay: Option<&Path>,
    format: Format,
) -> Result<(), Error> {
    let catalog = catalog.ok_or_else(|| {
        Error::InvalidArguments("--catalog is required for the overlay command".to_string())
    })?;
    let path = overlay_path(catalog, overlay);
    // Before the catalog loads, as with piped queries, so a malformed line
    // costs a diagnostic rather than a full load.
    let records = match action {
        OverlayAction::Record => {
            let mut stdin = std::io::stdin();
            if stdin.is_terminal() {
                return Err(Error::InvalidArguments(
                    "no records given: pipe them on stdin, one JSON object per line".to_string(),
                ));
            }
            let mut input = String::new();
            stdin.read_to_string(&mut input)?;
            parse_records(&input)?
        }
        OverlayAction::List | OverlayAction::Compact => Vec::new(),
    };

    let session = open_with_cache(catalog, facts_cache()?.as_ref())?;
    let mut overlay = Overlay::open(&session, &path)?;
    let text = match action {
        OverlayAction::Record => {
            let recorded = records.len();
            overlay.record_lines(&session, records)?;
            let saved = overlay.save()?;
            format!("recorded {recorded}, saved {saved} to {}\n", path.display())
        }
        OverlayAction::List => overlay_table(&overlay.summary(&session)),
        OverlayAction::Compact => {
            overlay.compact()?;
            format!(
                "compacted {}: {} edges\n",
                path.display(),
                overlay.edges().count()
            )
        }
    };
    match format {
        Format::Json => print_stdout(&format!(
            "{}\n",
            serde_json::to_string_pretty(&overlay.summary(&session))
                .map_err(|error| Error::InvalidArguments(error.to_string()))?
        )),
        Format::Text(_) => print_stdout(&text),
    }
}

/// One line per edge: `<key>  <confidence>  <verdict|unverified>  <n> site(s)`.
fn overlay_table(summary: &OverlaySummary) -> String {
    summary
        .edges
        .iter()
        .map(|entry| {
            let verdict = verdict_label(
                entry
                    .edge
                    .verification
                    .as_ref()
                    .map(|verification| verification.verdict),
            );
            format!(
                "{}  {}  {verdict}  {} site(s)\n",
                entry.edge.key, entry.edge.confidence, entry.sites
            )
        })
        .collect()
}

/// Answers the queries piped on stdin from one load of `catalog`. Reading
/// a terminal would wait for input nobody knows to type, so that is an
/// error; empty stdin answers nothing.
fn run_stdin_queries(
    catalog: &Path,
    overlay: Option<&Path>,
    modifiers: QueryModifiers,
    format: Format,
) -> Result<(), Error> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(Error::InvalidArguments(
            "no query given: name one, or pipe queries on stdin".to_string(),
        ));
    }
    let mut input = String::new();
    stdin.read_to_string(&mut input)?;
    let queries = parse_queries(&input, modifiers)?;
    if queries.is_empty() {
        return Ok(());
    }
    init_logging()?;
    let session = open_with_cache(catalog, facts_cache()?.as_ref())?;
    let named = overlay;
    let mut opened = None;
    for (line, query) in queries {
        let overlay = overlay_for(&mut opened, &session, catalog, named, &query)?;
        let result = run_with_overlay(&session, overlay, &query)?;
        let answer = match format {
            Format::Json => serde_json::to_string(&result)
                .map(|json| format!("{json}\n"))
                .map_err(|error| Error::InvalidArguments(error.to_string()))?,
            Format::Text(mode) => format!(
                "== {line}\n{}",
                rllvm_query::render(&result, mode, stdout_color())
            ),
        };
        print_stdout(&answer)?;
    }
    Ok(())
}

/// How answers are printed, chosen once for the whole run.
#[derive(Clone, Copy)]
enum Format {
    Json,
    Text(TextMode),
}

impl Format {
    fn of(args: &QueryArgs) -> Format {
        if args.json {
            Format::Json
        } else if args.full {
            Format::Text(TextMode::Full)
        } else {
            Format::Text(TextMode::Adaptive)
        }
    }
}

/// Matches the wrapper binaries' own convention (`rllvm_cc.rs`,
/// `rllvm_get_bc.rs`, `rllvm_rustc.rs`): the configured log level, on
/// stderr, so `tracing::warn!` (a module that failed to extract) actually
/// reaches a reader instead of being silently dropped by the default no-op
/// subscriber. Called only once a query is certain to run, so
/// `--llvm-version` alone never touches the configuration file.
fn init_logging() -> Result<(), Error> {
    FmtSubscriber::builder()
        .with_max_level(try_rllvm_config()?.log_level())
        .with_writer(std::io::stderr)
        .init();
    Ok(())
}

fn run_query(args: QueryArgs) -> Result<(), Error> {
    let format = Format::of(&args);
    let Some(command) = args.command else {
        // With a catalog and no query, the queries arrive on stdin.
        return match &args.catalog {
            Some(catalog) => {
                run_stdin_queries(catalog, args.overlay.as_deref(), args.modifiers, format)
            }
            None => Ok(()),
        };
    };

    init_logging()?;

    // The dispatch, named rather than inferred from `to_query`'s `None`.
    // Two commands answer `None` now -- `Mcp` and `Completions` -- so `None`
    // no longer identifies which mode was asked for, and reading it as "serve
    // MCP" would make the next mode-shaped variant silently do that: the one
    // outcome nobody would think to test for. Exhaustive and wildcard-free,
    // so such a variant has to fail to compile here too, not only in
    // `to_query`, where its `None` arm would otherwise be the whole story.
    match &command {
        QueryCommand::Mcp => return serve_mcp(args.catalog.as_deref()),
        // `main` answers this one before `run_query` is reached, so arriving
        // here means that early return was dropped --
        // `completions_name_the_query_binary` fails when it is, because
        // nothing reaches stdout.
        QueryCommand::Completions { .. } => return Ok(()),
        QueryCommand::Cache { action } => {
            if args.catalog.is_some() {
                return Err(Error::InvalidArguments(
                    "the cache command reads no catalog; drop --catalog".into(),
                ));
            }
            return run_cache(action.as_ref(), format);
        }
        QueryCommand::Overlay { action } => {
            return run_overlay(
                action,
                args.catalog.as_deref(),
                args.overlay.as_deref(),
                format,
            );
        }
        QueryCommand::Defs { .. }
        | QueryCommand::At { .. }
        | QueryCommand::Callers { .. }
        | QueryCommand::Callees { .. }
        | QueryCommand::Uses { .. }
        | QueryCommand::Reach { .. }
        | QueryCommand::Closure { .. }
        | QueryCommand::Externals
        | QueryCommand::FfiExports
        | QueryCommand::IndirectTargets { .. }
        | QueryCommand::ResolutionCandidates => {}
    }

    // Unreachable through the match above, which returns for every command
    // `to_query` answers `None` for. An error rather than a panic: a binary
    // that mis-dispatches should say so, not abort.
    let Some(query) = to_query(command, args.modifiers) else {
        return Err(Error::InvalidArguments(
            "this command names a mode, not a query".to_string(),
        ));
    };

    // Before `open`, so a mistyped location costs a diagnostic rather than a
    // full read and extraction of the catalog.
    query.validate()?;
    let catalog = args.catalog.ok_or_else(|| {
        Error::InvalidArguments("--catalog is required to run a query".to_string())
    })?;
    let session = open_with_cache(&catalog, facts_cache()?.as_ref())?;
    let mut opened = None;
    let overlay = overlay_for(
        &mut opened,
        &session,
        &catalog,
        args.overlay.as_deref(),
        &query,
    )?;
    let result = run_with_overlay(&session, overlay, &query)?;

    match format {
        Format::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|error| Error::InvalidArguments(error.to_string()))?;
            print_stdout(&format!("{json}\n"))?;
        }
        Format::Text(mode) => {
            print_stdout(&rllvm_query::render(&result, mode, stdout_color()))?;
        }
    }
    Ok(())
}

/// Whether stdout should be coloured. `supports_color::on` honours
/// `NO_COLOR`, `FORCE_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM=dumb`,
/// `COLORTERM` and `TERM_PROGRAM`, none of which a bare `is_terminal()`
/// check reads. Its CI detection cannot change a piped run's answer: the
/// terminal test short-circuits to "no colour" before `is_ci` is reached,
/// so CI only raises the level of a stream that is already a terminal.
///
/// Sensed here and passed to `render` rather than called inside it: `on`
/// reads the real environment, so an inline `if_supports_color` would make
/// the renderer's unit tests depend on whether the suite ran under
/// `--nocapture` on a terminal.
fn stdout_color() -> Color {
    match supports_color::on(supports_color::Stream::Stdout) {
        Some(_) => Color::Always,
        None => Color::Never,
    }
}

/// Serves MCP over stdio. `--catalog` is optional here and only preloads:
/// the point of the server is that a client chooses what to analyze, through
/// `load_catalog` and `inventory`, and keeps several catalogs loaded at once.
/// A preload failure is still fatal -- a client that asked for a catalog on
/// the command line should hear that it could not be read, not discover it
/// one query later.
fn serve_mcp(catalog: Option<&std::path::Path>) -> Result<(), Error> {
    let mut registry = mcp::Registry::with_cache(facts_cache()?);
    if let Some(catalog) = catalog {
        registry.load(catalog)?;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    mcp::serve(&mut registry, stdin.lock(), stdout.lock())
}

fn main() -> ExitCode {
    let args = QueryArgs::parse();
    if args.llvm_version {
        // Documented as "print ... and exit": must return here rather than
        // falling into `run_query`, or `--llvm-version --catalog c mcp`
        // would print a bare version line onto stdout ahead of the
        // JSON-RPC frames, corrupting the protocol stream.
        return rllvm_core::error::report(print_stdout(&format!("{}\n", llvm_version())));
    }
    // Also before any configuration is read: generating a completion script
    // is a property of the CLI definition alone, and must not fail on a
    // machine that has no usable LLVM configuration yet.
    if let Some(QueryCommand::Completions { shell }) = args.command {
        let mut command = QueryArgs::command();
        let name = command.get_name().to_string();
        clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    match run_query(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rllvm_query::Confidence;

    /// The reverse of `to_query`'s exhaustiveness: every [`Query`]
    /// variant must appear here, with no wildcard arm. A `Query` variant
    /// added without a matching arm fails to compile, so a new query cannot
    /// ship without a `QueryCommand` (and a `to_query` arm) to drive it from
    /// the command line -- closing the drift gap `to_query` alone leaves
    /// open, since it is only ever exhaustive over `QueryCommand`.
    fn cli_command_for(query: &Query) -> QueryCommand {
        match query {
            Query::Defs { name } => QueryCommand::Defs { name: name.clone() },
            Query::At { file, line } => QueryCommand::At {
                file: file.clone(),
                line: *line,
            },
            Query::Callers { name } => QueryCommand::Callers { name: name.clone() },
            Query::Callees { name } => QueryCommand::Callees { name: name.clone() },
            Query::Uses { name } => QueryCommand::Uses { name: name.clone() },
            Query::Reach { from, to, .. } => QueryCommand::Reach {
                from: from.clone(),
                to: to.clone(),
            },
            Query::Closure {
                name, direction, ..
            } => QueryCommand::Closure {
                name: name.clone(),
                direction: match direction {
                    Direction::In => ClosureDirection::In,
                    Direction::Out => ClosureDirection::Out,
                },
            },
            Query::Externals => QueryCommand::Externals,
            Query::FfiExports => QueryCommand::FfiExports,
            Query::IndirectTargets { at, .. } => QueryCommand::IndirectTargets { at: at.clone() },
            Query::ResolutionCandidates => QueryCommand::ResolutionCandidates,
        }
    }

    #[test]
    fn each_line_of_stdin_is_one_query_in_command_line_syntax() {
        let queries = parse_queries(
            "callees quiche_accept\n\n  defs 'int twice<int>(int)'\nindirect-targets t.c:4 --heuristics\n",
            QueryModifiers::default(),
        )
        .unwrap();
        let lines: Vec<&str> = queries.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "callees quiche_accept",
                "defs 'int twice<int>(int)'",
                "indirect-targets t.c:4 --heuristics"
            ]
        );
        assert_eq!(
            format!("{:?}", queries[1].1),
            format!(
                "{:?}",
                Query::Defs {
                    name: "int twice<int>(int)".into()
                }
            )
        );
        assert!(matches!(
            queries[2].1,
            Query::IndirectTargets {
                heuristics: true,
                ..
            }
        ));
    }

    #[test]
    fn heuristics_on_the_command_line_reach_every_stdin_query() {
        let heuristics = QueryModifiers {
            heuristics: true,
            ..QueryModifiers::default()
        };
        let queries = parse_queries("indirect-targets t.c:4\n", heuristics).unwrap();
        assert!(matches!(
            queries[0].1,
            Query::IndirectTargets {
                heuristics: true,
                ..
            }
        ));
    }

    #[test]
    fn a_stdin_line_may_ask_for_the_overlay() {
        let queries = parse_queries(
            "reach main handler --include-overlay --min-confidence medium\nreach main handler\n",
            QueryModifiers::default(),
        )
        .unwrap();
        assert!(matches!(
            queries[0].1,
            Query::Reach {
                include_overlay: true,
                min_confidence: Some(Confidence::Medium),
                ..
            }
        ));
        assert!(queries[0].1.walks_overlay());
        assert!(!queries[1].1.walks_overlay(), "each line asks for itself");

        let error = parse_queries(
            "reach main handler --min-confidence high\n",
            QueryModifiers::default(),
        )
        .map(|_| ())
        .unwrap_err()
        .to_string();
        assert!(error.contains("line 1:"), "{error}");
    }

    #[test]
    fn a_bad_stdin_line_is_reported_by_its_number() {
        for (input, expected) in [
            ("callees a\nfrobnicate b\n", "line 2:"),
            ("\ncallees a --json\n", "line 2:"),
            ("callees a\n--catalog c.json callees b\n", "line 2:"),
            ("mcp\n", "line 1:"),
            ("completions bash\n", "line 1:"),
            ("callees a\ncallees b\nindirect-targets main.c\n", "line 3:"),
        ] {
            let error = parse_queries(input, QueryModifiers::default())
                .map(|_| ())
                .expect_err(input)
                .to_string();
            assert!(error.contains(expected), "{input:?} gave {error:?}");
            assert_eq!(error.lines().count(), 1, "{input:?} gave {error:?}");
        }
    }

    #[test]
    fn empty_stdin_names_no_query() {
        assert!(
            parse_queries("", QueryModifiers::default())
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_queries("\n  \n", QueryModifiers::default())
                .unwrap()
                .is_empty()
        );
    }

    /// `to_query` maps `QueryCommand::Mcp` to `None`, since it selects a
    /// mode rather than naming a query.
    #[test]
    fn the_mcp_command_has_no_query() {
        assert!(to_query(QueryCommand::Mcp, QueryModifiers::default()).is_none());
    }

    /// Same for `Completions`: the binary answers it before a query could
    /// run, so reaching `to_query` with it must not name one.
    #[test]
    fn the_completions_command_has_no_query() {
        let command = QueryCommand::Completions {
            shell: clap_complete::Shell::Bash,
        };
        assert!(to_query(command, QueryModifiers::default()).is_none());
    }

    #[test]
    fn the_overlay_command_has_no_query() {
        let command = QueryCommand::Overlay {
            action: OverlayAction::List,
        };
        assert!(to_query(command, QueryModifiers::default()).is_none());
    }

    #[test]
    fn a_bad_record_on_stdin_is_reported_by_its_number() {
        let good = r#"{"op":"retract","edge":{"via_field":{"record":"ops","offset":8},"to":{"module_id":"m","symbol":"h3"}},"reason":"r"}"#;
        let lines: Vec<usize> = parse_records(&format!("\n{good}\n\n{good}\n"))
            .unwrap()
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        assert_eq!(lines, [2, 4], "each record keeps its line for validation");
        for (input, expected) in [
            (format!("{good}\n{{\n"), "line 2:"),
            (format!("\n{good}\n{{\"op\":\"annotate\"}}\n"), "line 3:"),
        ] {
            let error = parse_records(&input).map(|_| ()).unwrap_err().to_string();
            assert!(error.contains(expected), "{input:?} gave {error:?}");
        }
    }

    /// Not just a compile-time fence: proves `to_query` and `cli_command_for`
    /// actually agree on every field, for every variant, not merely that
    /// both happen to be exhaustive.
    #[test]
    fn every_query_variant_round_trips_through_the_cli_command_mapping() {
        let modifiers = QueryModifiers {
            heuristics: true,
            include_overlay: true,
            min_confidence: Some(Confidence::High),
        };
        let queries = [
            Query::Defs { name: "f".into() },
            Query::At {
                file: "t.c".into(),
                line: 1,
            },
            Query::Callers { name: "f".into() },
            Query::Callees { name: "f".into() },
            Query::Uses { name: "f".into() },
            Query::Reach {
                from: "a".into(),
                to: "b".into(),
                include_overlay: true,
                min_confidence: Some(Confidence::High),
            },
            Query::Closure {
                name: "f".into(),
                direction: Direction::In,
                include_overlay: true,
                min_confidence: Some(Confidence::High),
            },
            Query::Closure {
                name: "f".into(),
                direction: Direction::Out,
                include_overlay: true,
                min_confidence: Some(Confidence::High),
            },
            Query::Externals,
            Query::FfiExports,
            Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics: true,
            },
        ];
        for query in queries {
            let command = cli_command_for(&query);
            let round_tripped =
                to_query(command, modifiers).expect("every QueryCommand but Mcp names a query");
            assert_eq!(
                format!("{round_tripped:?}"),
                format!("{query:?}"),
                "CLI round-trip must preserve every field"
            );
        }
    }
}
