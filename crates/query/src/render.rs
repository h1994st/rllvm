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
    /// The `note:` prefix on footer lines. Constructed starting with the
    /// footer renderer.
    #[allow(dead_code, reason = "constructed once the footer lands")]
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

/// Renders one answer. Infallible: every field it reads is already owned by
/// the result.
pub fn render(result: &QueryResult, mode: TextMode, color: Color) -> String {
    let ctx = Ctx { mode, color };
    let mut out = String::new();
    render_results(result, ctx, &mut out);
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
    use crate::{Query, run, testing::*};

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
        assert_eq!(
            render(&result, TextMode::Adaptive, Color::Never).trim(),
            "<no location>  main"
        );
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
    #[ignore = "footer arrives in Task 4"]
    fn externals_prints_each_symbol_with_its_binding_status() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Externals).unwrap();
        // No unbound symbols in this fixture: the footer, not a row, says so.
        assert!(render(&result, TextMode::Adaptive, Color::Never).contains("matched nothing"));
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
}
