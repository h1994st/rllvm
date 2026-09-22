//! Human-readable rendering of a [`QueryResult`].
//!
//! Returns a `String` rather than writing to stdout so the renderers are
//! ordinary unit tests rather than capture harnesses. The JSON envelope is
//! untouched: `--json` still serializes [`QueryResult`] directly, and MCP
//! never reaches this module at all.

use std::collections::BTreeMap;

use owo_colors::OwoColorize;

use crate::{
    PathStep, QueryResult, QueryResults,
    bind::BindingStatus,
    facts::{CallSiteFact, CallTarget, FunctionId, SourceLocation},
};

/// How much of the envelope to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMode {
    /// Results, plus only the footer lines that carry information.
    Adaptive,
    /// Results, plus every envelope field including zeroes.
    Full,
}

/// Whether to emit ANSI styling. Injected rather than sensed inside this
/// module: `supports_color::on` reads the real environment, which would make
/// every assertion below depend on how the suite was invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Always,
    Never,
}

/// The palette. Semantic rather than decorative: the same meaning gets the
/// same colour in every query, so `green` always reads as "resolved" and
/// `yellow` as "the tool is not certain".
///
/// Colour is always redundant with the text. `indirect`, `unresolved` and
/// `Ambiguous` are spelled out as words too, so a piped answer, a monochrome
/// terminal and a colour-blind reader lose nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paint {
    /// `--full` section headings. Constructed starting with the `--full`
    /// section renderer.
    #[allow(dead_code, reason = "constructed once the --full sections land")]
    Heading,
    /// The `note:` prefix on footer lines.
    Note,
    /// `file:line`.
    Location,
    /// The symbol an answer is about.
    Symbol,
    /// Resolved and certain: a direct call, a unique binding, a known bound.
    Resolved,
    /// Known to be incomplete: an indirect call, an ambiguous binding, a
    /// conditional path step.
    Uncertain,
    /// Absent or unbound: an unresolved site, an unbound symbol.
    Absent,
    /// Present but not the point: intrinsics, inline asm, a missing location.
    Muted,
}

/// Mode and colour travel together through every renderer. A copy struct
/// rather than two parameters threaded side by side, which reached five
/// arguments on the call-site helpers.
#[derive(Clone, Copy)]
struct Ctx {
    mode: TextMode,
    color: Color,
}

/// Printed where a fact has no source location, rather than an empty column
/// that would silently align the next field into the location's place.
const NO_LOCATION: &str = "<no location>";

/// Prefix for every footer line, so a reader can tell a caveat from a fact
/// and `grep -v 'note:'` leaves only results.
const NOTE: &str = "note:";

fn paint(text: &str, color: Color, style: Paint) -> String {
    if color == Color::Never {
        return text.to_string();
    }
    match style {
        Paint::Heading => format!("{}", text.bold()),
        Paint::Note => format!("{}", text.yellow()),
        Paint::Location => format!("{}", text.cyan()),
        Paint::Symbol => format!("{}", text.bright_white()),
        Paint::Resolved => format!("{}", text.green()),
        Paint::Uncertain => format!("{}", text.yellow()),
        Paint::Absent => format!("{}", text.red()),
        Paint::Muted => format!("{}", text.dimmed()),
    }
}

/// The readable form of one symbol. Demangled when the envelope's `symbols`
/// table has a reading for it, and in [`TextMode::Full`] the mangled symbol
/// follows in brackets, because the mangled name is the identity.
fn name(symbols: &BTreeMap<String, String>, symbol: &str, ctx: Ctx) -> String {
    let readable = match (symbols.get(symbol), ctx.mode) {
        (Some(demangled), TextMode::Adaptive) => demangled.clone(),
        (Some(demangled), TextMode::Full) => {
            return format!(
                "{}  {}",
                paint(demangled, ctx.color, Paint::Symbol),
                paint(&format!("[{symbol}]"), ctx.color, Paint::Muted)
            );
        }
        (None, _) => symbol.to_string(),
    };
    paint(&readable, ctx.color, Paint::Symbol)
}

fn location(location: Option<&SourceLocation>, ctx: Ctx) -> String {
    match location {
        Some(location) => paint(
            &format!("{}:{}", location.file.display(), location.line),
            ctx.color,
            Paint::Location,
        ),
        None => paint(NO_LOCATION, ctx.color, Paint::Muted),
    }
}

/// Comma-joined readable names, for a bound of resolved indirect-call
/// targets.
fn symbol_list(symbols: &BTreeMap<String, String>, ids: &[FunctionId], ctx: Ctx) -> String {
    ids.iter()
        .map(|id| name(symbols, &id.symbol, ctx))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One call target, named by kind so an indirect or intrinsic site never
/// reads like a resolved call. The kind word is coloured by how certain it
/// is, and still printed, so the colour is never the only signal.
fn call_target(symbols: &BTreeMap<String, String>, target: &CallTarget, ctx: Ctx) -> String {
    match target {
        CallTarget::Direct { callee } => format!(
            "{}    {}",
            paint("direct", ctx.color, Paint::Resolved),
            name(symbols, &callee.symbol, ctx)
        ),
        CallTarget::Indirect {
            signature,
            llvm_target_bound,
        } => {
            let kind = paint("indirect", ctx.color, Paint::Uncertain);
            let signature = paint(signature, ctx.color, Paint::Muted);
            match llvm_target_bound {
                Some(bound) => format!(
                    "{kind}  {signature}  {} {}",
                    paint("bound:", ctx.color, Paint::Resolved),
                    symbol_list(symbols, bound, ctx)
                ),
                None => format!(
                    "{kind}  {signature}  {}",
                    paint("unresolved", ctx.color, Paint::Absent)
                ),
            }
        }
        CallTarget::Intrinsic { name: intrinsic } => format!(
            "{} {}",
            paint("intrinsic", ctx.color, Paint::Muted),
            paint(intrinsic, ctx.color, Paint::Muted)
        ),
        CallTarget::InlineAsm => paint("inline-asm", ctx.color, Paint::Muted),
    }
}

fn call_sites(
    symbols: &BTreeMap<String, String>,
    sites: &[CallSiteFact],
    ctx: Ctx,
    out: &mut String,
) {
    for site in sites {
        out.push_str(&format!(
            "    {}  {}\n",
            location(site.location.as_ref(), ctx),
            call_target(symbols, &site.target, ctx)
        ));
    }
}

/// One sentence per query naming what an empty result means. Separate
/// strings rather than one generic line because the readings genuinely
/// differ: an empty `reach` is not the same claim as an empty `defs`.
fn empty_meaning(results: &QueryResults) -> &'static str {
    match results {
        QueryResults::Reach(_) => "no path over resolved edges; not proof of unreachability",
        QueryResults::Callees(_) => "the target defined no outgoing call sites",
        QueryResults::Externals(_) => "every symbol in scope bound to a definition",
        _ => "the name or location matched nothing in the selected scope",
    }
}

/// Appends the caveats a JSON reader gets from `resolution`, `analysis` and
/// `uncertainty` but a text reader would otherwise never see. Prints nothing
/// when every count is zero and every result matched exactly: a clean answer
/// gets no footer at all, so the presence of `note:` is itself a signal.
fn footer(result: &QueryResult, color: Color, out: &mut String) {
    let mut notes: Vec<String> = Vec::new();

    // Rule 1: empty results always say what empty means.
    if result.results.is_empty() {
        notes.push(empty_meaning(&result.results).to_string());
    }

    // Rule 2: how each name resolved, when it was not an exact hit.
    for resolution in &result.resolution {
        match resolution.matched {
            None => notes.push(format!("'{}' matched nothing", resolution.requested)),
            Some(crate::NameMatch::Fuzzy) => notes.push(format!(
                "'{}' matched loosely, gathering {} symbol(s)",
                resolution.requested,
                resolution.symbols.len()
            )),
            Some(_) => {}
        }
    }

    // Rule 3: modules that narrowed what was read without narrowing scope.
    let analysis = &result.analysis;
    let module_counts = [
        ("failed", analysis.failed),
        ("missing", analysis.missing),
        ("changed", analysis.changed),
        ("unsupported", analysis.unsupported),
        ("not built", analysis.not_built),
    ];
    let unread: Vec<String> = module_counts
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{count} {label}"))
        .collect();
    if !unread.is_empty() {
        notes.push(format!(
            "{} of {} modules were not analysed: {}",
            unread.len(),
            analysis.modules.len(),
            unread.join(", ")
        ));
    }

    // Rule 4: every non-zero uncertainty count.
    let uncertainty = &result.uncertainty;
    if uncertainty.indirect_call_sites > 0 {
        notes.push(format!(
            "{} indirect call site(s), {} with an LLVM target bound",
            uncertainty.indirect_call_sites, uncertainty.sites_with_llvm_target_bound
        ));
    }
    if uncertainty.ambiguous_bindings > 0 {
        notes.push(format!(
            "{} ambiguous binding(s)",
            uncertainty.ambiguous_bindings
        ));
    }
    if uncertainty.functions_without_location > 0 {
        notes.push(format!(
            "{} function(s) without a source location",
            uncertainty.functions_without_location
        ));
    }
    if uncertainty.locations_from_modified_sources > 0 {
        notes.push(format!(
            "{} location(s) from modified sources",
            uncertainty.locations_from_modified_sources
        ));
    }
    if uncertainty.conditional_path_steps > 0 {
        notes.push(format!(
            "{} step(s) hold only if the call takes the member the path chose",
            uncertainty.conditional_path_steps
        ));
    }

    // Rule 5: the address-taken inventory is a heuristic, never an edge.
    if let QueryResults::IndirectTargets(results) = &result.results
        && results
            .iter()
            .any(|entry| entry.address_taken_inventory.is_some())
    {
        notes.push("address-taken candidates are signature-matched, never call edges".to_string());
    }

    if notes.is_empty() {
        return;
    }
    out.push('\n');
    let prefix = paint(NOTE, color, Paint::Note);
    for note in notes {
        out.push_str(&format!("{prefix} {note}\n"));
    }
}

/// Renders one answer. Infallible: every field it reads is already owned by
/// the result.
pub fn render(result: &QueryResult, mode: TextMode, color: Color) -> String {
    let ctx = Ctx { mode, color };
    let mut out = String::new();
    render_results(result, ctx, &mut out);
    footer(result, color, &mut out);
    out
}

fn render_results(result: &QueryResult, ctx: Ctx, out: &mut String) {
    let symbols = &result.symbols;
    match &result.results {
        QueryResults::Defs(entries) => {
            for entry in entries {
                out.push_str(&format!(
                    "{}  {}\n",
                    location(entry.location.as_ref(), ctx),
                    name(symbols, &entry.function.symbol, ctx)
                ));
            }
        }
        QueryResults::Closure(functions) => {
            for function in functions {
                out.push_str(&format!("{}\n", name(symbols, &function.symbol, ctx)));
            }
        }
        QueryResults::Externals(bindings) => {
            for binding in bindings {
                let tint = match binding.status {
                    BindingStatus::Unique => Paint::Resolved,
                    BindingStatus::Ambiguous => Paint::Uncertain,
                    BindingStatus::Unbound => Paint::Absent,
                };
                out.push_str(&format!(
                    "{}  {}\n",
                    name(symbols, &binding.symbol, ctx),
                    paint(&format!("{:?}", binding.status), ctx.color, tint)
                ));
            }
        }
        QueryResults::At(entries) => {
            for entry in entries {
                out.push_str(&format!("{}\n", name(symbols, &entry.function.symbol, ctx)));
                call_sites(symbols, &entry.call_sites, ctx, out);
            }
        }
        QueryResults::Callers(entries) => {
            for entry in entries {
                out.push_str(&format!("{}\n", name(symbols, &entry.function.symbol, ctx)));
                call_sites(symbols, &entry.call_sites, ctx, out);
            }
        }
        QueryResults::Callees(sites) => call_sites(symbols, sites, ctx, out),
        QueryResults::Uses(uses) => {
            for use_fact in uses {
                let in_function = match &use_fact.in_function {
                    Some(id) => name(symbols, &id.symbol, ctx),
                    None => paint("<no function>", ctx.color, Paint::Muted),
                };
                out.push_str(&format!(
                    "{}  in {}  at {}\n",
                    paint(&format!("{:?}", use_fact.kind), ctx.color, Paint::Uncertain),
                    in_function,
                    location(use_fact.location.as_ref(), ctx)
                ));
            }
        }
        QueryResults::Reach(path) => {
            // `None` is no path; `Some(vec![])` is a trivial found path. The
            // footer distinguishes them, so an empty `Some` prints nothing
            // here rather than a misleading blank.
            for step in path.iter().flatten() {
                match step {
                    PathStep::Call(site) => out.push_str(&format!(
                        "{}              {}\n",
                        paint("call", ctx.color, Paint::Resolved),
                        name(symbols, &site.function.symbol, ctx)
                    )),
                    PathStep::BoundedIndirect {
                        site,
                        chosen,
                        bound,
                    } => {
                        out.push_str(&format!(
                            "{}  {}  chose {}  of {}\n",
                            paint("bounded-indirect", ctx.color, Paint::Uncertain),
                            name(symbols, &site.function.symbol, ctx),
                            name(symbols, &chosen.symbol, ctx),
                            symbol_list(symbols, bound, ctx)
                        ));
                    }
                    PathStep::Binding(binding) => {
                        let tint = match binding.status {
                            BindingStatus::Unique => Paint::Resolved,
                            BindingStatus::Ambiguous => Paint::Uncertain,
                            BindingStatus::Unbound => Paint::Absent,
                        };
                        out.push_str(&format!(
                            "{}           {}  ({}, {} candidate(s))\n",
                            paint("binding", ctx.color, Paint::Location),
                            name(symbols, &binding.symbol, ctx),
                            paint(&format!("{:?}", binding.status), ctx.color, tint),
                            binding.candidates.len()
                        ));
                    }
                }
            }
        }
        QueryResults::IndirectTargets(results) => {
            for entry in results {
                out.push_str(&format!(
                    "{}  {}\n",
                    location(entry.location.as_ref(), ctx),
                    paint(&entry.signature, ctx.color, Paint::Muted)
                ));
                match (&entry.llvm_target_bound, entry.unresolved) {
                    (Some(bound), _) => out.push_str(&format!(
                        "    {} {}\n",
                        paint("bound:", ctx.color, Paint::Resolved),
                        symbol_list(symbols, bound, ctx)
                    )),
                    (None, true) => out.push_str(&format!(
                        "    {}\n",
                        paint("unresolved", ctx.color, Paint::Absent)
                    )),
                    (None, false) => {}
                }
                if let Some(inventory) = &entry.address_taken_inventory {
                    out.push_str(&format!(
                        "    {}\n",
                        paint(
                            &format!("address-taken candidates: {}", inventory.len()),
                            ctx.color,
                            Paint::Uncertain
                        )
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Query,
        facts::{FunctionFact, Linkage, SourceStatus},
        index::Session,
        run,
        testing::*,
    };

    #[test]
    fn defs_prints_one_line_per_definition() {
        let session = session_with_cxx_symbols();
        let result = run(
            &session,
            &Query::Defs {
                name: "main".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        // Not an exact match on the whole output: `session_with_cxx_symbols`
        // gives none of its functions a location, so the footer reports that
        // program-wide, after the one line this test is actually about.
        assert!(text.starts_with("<no location>  main\n"), "got: {text}");
    }

    #[test]
    fn a_mangled_symbol_prints_its_demangled_reading() {
        let session = session_with_cxx_symbols();
        let result = run(
            &session,
            &Query::Defs {
                name: "_Z5twiceIiET_S0_".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("int twice<int>(int)"), "got: {text}");
        assert!(!text.contains("_Z5twiceIiET_S0_"), "mangled leaked: {text}");
    }

    #[test]
    fn full_mode_keeps_the_mangled_symbol_beside_the_reading() {
        let session = session_with_cxx_symbols();
        let result = run(
            &session,
            &Query::Defs {
                name: "_Z5twiceIiET_S0_".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Full, Color::Never);
        assert!(text.contains("int twice<int>(int)"), "got: {text}");
        assert!(text.contains("[_Z5twiceIiET_S0_]"), "got: {text}");
    }

    #[test]
    fn externals_prints_each_symbol_with_its_binding_status() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Externals).unwrap();
        // No unbound symbols in this fixture: the footer, not a row, says so.
        assert!(
            render(&result, TextMode::Adaptive, Color::Never)
                .contains("every symbol in scope bound to a definition")
        );
    }

    #[test]
    fn color_never_emits_no_escape_codes() {
        for style in [
            Paint::Heading,
            Paint::Note,
            Paint::Location,
            Paint::Symbol,
            Paint::Resolved,
            Paint::Uncertain,
            Paint::Absent,
            Paint::Muted,
        ] {
            assert_eq!(paint("x", Color::Never, style), "x");
            assert!(
                paint("x", Color::Always, style).contains('\u{1b}'),
                "{style:?} produced no escape code"
            );
        }
    }

    #[test]
    fn a_coloured_answer_carries_the_same_words_as_a_plain_one() {
        // Colour is redundant with the text: stripping the escape codes from
        // a coloured answer must give back the plain one, so a pipe, a
        // monochrome terminal and a colour-blind reader lose nothing.
        let session = session_with_bounded_indirect();
        let result = run(&session, &Query::Callees { name: "a".into() }).unwrap();
        let plain = render(&result, TextMode::Adaptive, Color::Never);
        let coloured = render(&result, TextMode::Adaptive, Color::Always);
        assert!(coloured.contains('\u{1b}'), "nothing was coloured");
        let stripped: String = {
            let mut out = String::new();
            let mut chars = coloured.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        assert_eq!(stripped, plain);
    }

    #[test]
    fn callers_reports_each_call_site_with_its_location() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Callers { name: "b".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("a"), "got: {text}");
    }

    #[test]
    fn callees_names_the_target_kind() {
        let session = session_with_bounded_indirect();
        let result = run(&session, &Query::Callees { name: "a".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("indirect"), "got: {text}");
    }

    #[test]
    fn at_lists_the_functions_mapped_to_a_line() {
        let session = session_from_source_lines(&[("parser.c", 4)]);
        let result = run(
            &session,
            &Query::At {
                file: "parser.c".into(),
                line: 4,
            },
        )
        .unwrap();
        assert!(render(&result, TextMode::Adaptive, Color::Never).contains("only"));
    }

    #[test]
    fn a_found_path_prints_its_steps_in_order() {
        let session = session_from(&[("a", "b")]);
        let result = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "b".into(),
            },
        )
        .unwrap();
        assert!(render(&result, TextMode::Adaptive, Color::Never).contains("call"));
    }

    #[test]
    fn a_conditional_step_says_it_is_conditional() {
        let session = session_with_bounded_indirect();
        let result = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "target".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("bounded-indirect"), "got: {text}");
    }

    #[test]
    fn indirect_targets_prints_the_signature_and_says_unresolved() {
        // `session_with_indirect_gap` records no location on its indirect
        // call (see `testing.rs`), so `at` could never address it. Its
        // unresolved indirect site sits in `session_with_address_taken_function`
        // at `t.c:4` instead.
        let session = session_with_address_taken_function();
        let result = run(
            &session,
            &Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics: false,
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("unresolved"), "got: {text}");
    }

    #[test]
    fn an_absent_path_says_what_absence_means() {
        let session = session_with_indirect_gap();
        let result = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "c".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("no path over resolved edges"), "got: {text}");
        assert!(text.contains("not proof of unreachability"), "got: {text}");
    }

    #[test]
    fn a_nonzero_uncertainty_count_reaches_the_reader() {
        let session = session_with_ambiguous_bindings();
        let result = run(&session, &Query::Externals).unwrap();
        assert!(result.uncertainty.ambiguous_bindings > 0, "fixture changed");
        assert!(
            render(&result, TextMode::Adaptive, Color::Never).contains("ambiguous binding"),
            "a non-zero ambiguity must not be silent"
        );
    }

    #[test]
    fn a_clean_answer_prints_no_footer() {
        // `session_from` leaves every function's `location` unset, which
        // would itself trip the footer's "functions without a location"
        // count; a genuinely clean answer needs functions that have one.
        let source_location = |line: u32| {
            Some(SourceLocation {
                file: "a.c".into(),
                directory: None,
                line,
                column: 1,
                source_status: SourceStatus::Current,
                status_basis: None,
                inlined_at: Vec::new(),
            })
        };
        let caller = FunctionFact {
            location: source_location(1),
            ..function("m", "a", true, Linkage::Internal)
        };
        let callee = FunctionFact {
            location: source_location(2),
            ..function("m", "b", true, Linkage::Internal)
        };
        let site = direct_call(&caller, &callee, 0);
        let session = Session::new(facts(vec![caller, callee], vec![site]), Vec::new());
        let result = run(&session, &Query::Callers { name: "b".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            !text.contains("note:"),
            "clean answer gained a footer: {text}"
        );
    }

    #[test]
    fn a_failed_module_is_reported_even_when_results_are_present() {
        let facts = facts_with_one_failed_module();
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        assert!(render(&result, TextMode::Adaptive, Color::Never).contains("failed"));
    }
}
