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
    /// Results, plus the envelope's `scope`, `analysis`, `uncertainty` and
    /// `provenance` blocks, zero counts included. Not the whole envelope:
    /// `--json` is what reads that.
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
    Heading,
    /// The `note:` prefix on footer lines.
    Note,
    /// `file:line`.
    Location,
    /// The symbol an answer is about. Weight rather than a hue: it is the
    /// content of the answer, not one of the certainty tiers, and
    /// `bright_white` was near-invisible on a light background.
    /// `rllvm-info`'s diagnostics emphasize the same way.
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

/// `reach x x` answers `Some(vec![])`: a found path with no steps to print.
/// Rule 1 cannot cover it -- [`QueryResults::is_empty`] is correctly false
/// for a found answer -- so without a line of its own the whole command
/// emits nothing and reads as "unreachable", the opposite of what it found.
const ZERO_STEP_REACH: &str = "the origin is already the destination, reached in zero steps";

/// What an empty answer means for the queries that take a name or a location
/// and can simply fail to find it. Named rather than inlined in
/// [`empty_meaning`] because the footer has to recognise it: rule 2 says the
/// same thing in the requester's own words, and printing both is one caveat
/// twice.
const MATCHED_NOTHING: &str = "the name or location matched nothing in the selected scope";

fn paint(text: &str, color: Color, style: Paint) -> String {
    if color == Color::Never {
        return text.to_string();
    }
    match style {
        Paint::Heading => format!("{}", text.bold()),
        Paint::Note => format!("{}", text.yellow()),
        Paint::Location => format!("{}", text.cyan()),
        Paint::Symbol => format!("{}", text.bold()),
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

/// One level of nesting, for call sites printed under the function that owns
/// them. `callees` prints no parent line, so its rows take [`NO_INDENT`] and
/// start at column 0 like every other query's results.
const CALL_SITE_INDENT: &str = "    ";
const NO_INDENT: &str = "";

fn call_sites(
    symbols: &BTreeMap<String, String>,
    sites: &[CallSiteFact],
    indent: &str,
    ctx: Ctx,
    out: &mut String,
) {
    for site in sites {
        out.push_str(&format!(
            "{indent}{}  {}\n",
            location(site.location.as_ref(), ctx),
            call_target(symbols, &site.target, ctx)
        ));
    }
}

/// What this one answer actually shows, for the two footer rules whose
/// counts are program-wide rather than per-answer.
///
/// `uncertainty.functions_without_location` and
/// `uncertainty.indirect_call_sites` are computed over the whole session.
/// Every `extern` declaration is a function without a location, so on any
/// multi-translation-unit program both counts are non-zero and both notes
/// print under every answer -- a constant that buries the load-bearing
/// notes and trains a reader to skip the footer. Gated on what the answer
/// in front of the reader actually shows, they carry information again; the
/// program-wide totals stay in `--full` and in the JSON envelope.
///
/// This decides `functions_without_location` alone. It is only half of what
/// decides `indirect_call_sites`, because an answer can be incomplete
/// *because of* an indirect site it never displays -- see
/// [`walks_call_edges`].
#[derive(Clone, Copy, Default)]
struct Shown {
    /// At least one [`NO_LOCATION`] was printed.
    missing_location: bool,
    /// At least one indirect call site was printed.
    indirect_call_site: bool,
}

fn shown_in_sites(sites: &[CallSiteFact], shown: &mut Shown) {
    for site in sites {
        shown.missing_location |= site.location.is_none();
        shown.indirect_call_site |= matches!(site.target, CallTarget::Indirect { .. });
    }
}

/// Mirrors [`render_results`] arm for arm, and is wildcard-free for the
/// same reason [`empty_meaning`] is: a new [`QueryResults`] variant has to
/// state what it shows rather than inherit "neither".
fn shown_in(results: &QueryResults) -> Shown {
    let mut shown = Shown::default();
    match results {
        QueryResults::Defs(entries) => {
            shown.missing_location = entries.iter().any(|entry| entry.location.is_none());
        }
        QueryResults::At(entries) => {
            for entry in entries {
                shown_in_sites(&entry.call_sites, &mut shown);
            }
        }
        QueryResults::Callers(entries) => {
            for entry in entries {
                shown_in_sites(&entry.call_sites, &mut shown);
            }
        }
        QueryResults::Callees(sites) => shown_in_sites(sites, &mut shown),
        QueryResults::Uses(uses) => {
            shown.missing_location = uses.iter().any(|use_fact| use_fact.location.is_none());
        }
        QueryResults::Reach(path) => {
            // A path prints no locations, and its only indirect step is the
            // one CVP bounded.
            shown.indirect_call_site = path
                .iter()
                .flatten()
                .any(|step| matches!(step, PathStep::BoundedIndirect { .. }));
        }
        QueryResults::IndirectTargets(entries) => {
            shown.missing_location = entries.iter().any(|entry| entry.location.is_none());
            // Every row of this answer is an indirect call site by
            // construction; the word itself is in the query's name rather
            // than the row.
            shown.indirect_call_site = !entries.is_empty();
        }
        // Neither prints a location nor a call site: `closure` prints bare
        // names, `externals` a symbol and its binding status.
        QueryResults::Closure(_) | QueryResults::Externals(_) => {}
    }
    shown
}

/// Whether an unresolved indirect call site can hide an edge this answer
/// depended on, whichever rows it happens to display.
///
/// `Session::callers` resolves a direct call and a CVP-bounded one and
/// nothing else, so an unbounded indirect site is precisely the edge a
/// walk over call edges is missing: `callers` whose displayed rows are all
/// direct, and `closure`, which prints bare names and shows no site at all,
/// are exactly the answers [`Shown`] would silence while the count bears on
/// them hardest. `defs`, `externals` and `uses` are not walks over call
/// edges, so the count cannot change what they mean and stays gated. `at`,
/// `callees` and `indirect-targets` display their own call sites, so
/// [`Shown`] already answers for them.
///
/// Wildcard-free like [`empty_meaning`]: a new [`QueryResults`] variant has
/// to state whether it walks edges rather than inherit "no".
fn walks_call_edges(results: &QueryResults) -> bool {
    match results {
        QueryResults::Callers(_) | QueryResults::Closure(_) | QueryResults::Reach(_) => true,
        QueryResults::Defs(_)
        | QueryResults::At(_)
        | QueryResults::Callees(_)
        | QueryResults::Uses(_)
        | QueryResults::Externals(_)
        | QueryResults::IndirectTargets(_) => false,
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
        // `Callers`, `Uses` and `Closure` all take a name that can resolve
        // perfectly well and still return no results, so the catch-all
        // below ("matched nothing") would be false: rule 2 stays silent in
        // that case because the name genuinely did match.
        QueryResults::Callers(_) => "no call to the target was found in the selected scope",
        QueryResults::Uses(_) => "no non-call use of the target was found in the selected scope",
        QueryResults::Closure(_) => {
            "no functions were found in the selected scope for that direction"
        }
        // Accurate for `Defs`, `At` and `IndirectTargets`: each takes a name
        // or a location and an empty result there really does mean nothing
        // matched. Spelled out rather than left to a wildcard, so a new
        // `QueryResults` variant has to choose its own reading instead of
        // silently inheriting this one.
        QueryResults::Defs(_) | QueryResults::At(_) | QueryResults::IndirectTargets(_) => {
            MATCHED_NOTHING
        }
    }
}

/// Appends the caveats a JSON reader gets from `resolution`, `analysis` and
/// `uncertainty` but a text reader would otherwise never see. Prints nothing
/// when every count is zero and every result matched exactly: a clean answer
/// gets no footer at all, so the presence of `note:` is itself a signal.
fn footer(result: &QueryResult, color: Color, out: &mut String) {
    let mut notes: Vec<String> = Vec::new();

    // Rule 2 names an unresolved request in the requester's own words, and
    // `MATCHED_NOTHING` says the same thing generically: one caveat, printed
    // once.
    let unresolved_name = result
        .resolution
        .iter()
        .any(|resolution| resolution.matched.is_none());

    // Rule 1: empty results always say what empty means.
    if result.results.is_empty() {
        let meaning = empty_meaning(&result.results);
        if !(unresolved_name && meaning == MATCHED_NOTHING) {
            notes.push(meaning.to_string());
        }
    }

    // Rule 1's sibling: a found path that prints no steps. Not an empty
    // answer, so rule 1 leaves it alone, and not a silent one either.
    if let QueryResults::Reach(Some(steps)) = &result.results
        && steps.is_empty()
    {
        notes.push(ZERO_STEP_REACH.to_string());
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
    // `verified` counts too: only `analyzed` means a module's facts were
    // actually extracted, and `Analysis::verified`'s own doc warns that
    // leaving it uncounted would let a module disappear from the summary.
    let analysis = &result.analysis;
    let module_counts = [
        ("verified", analysis.verified),
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
        // The count of modules not analysed, not the count of non-zero
        // categories: 3 failed + 2 missing is 5 unread modules, and
        // `unread.len()` (2, one per category) would understate that.
        let unread_modules: usize = module_counts.iter().map(|(_, count)| count).sum();
        notes.push(format!(
            "{} of {} modules were not analysed: {}",
            unread_modules,
            analysis.modules.len(),
            unread.join(", ")
        ));
    }

    // Rule 4: every non-zero uncertainty count. The two counts that are
    // program-wide rather than per-answer print only when they can bear on
    // this answer: because it shows the thing they count (see [`Shown`]),
    // or, for indirect sites, because it walked the edges one of them could
    // hide (see [`walks_call_edges`]). The count itself stays program-wide:
    // having seen one, the reader is told how many there are.
    let shown = shown_in(&result.results);
    let uncertainty = &result.uncertainty;
    if uncertainty.indirect_call_sites > 0
        && (shown.indirect_call_site || walks_call_edges(&result.results))
    {
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
    if uncertainty.functions_without_location > 0 && shown.missing_location {
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

    // Rule 6: an LLVM target bound is sound only inside what was captured.
    // A JSON reader is told so by `IndirectTargetsResult::assumptions`;
    // without this the text reader is handed the bound as if it were
    // absolute. Quoted from the answer rather than restated here, so an
    // assumption added to that field cannot go silently missing from text.
    if let QueryResults::IndirectTargets(results) = &result.results {
        for assumption in results
            .iter()
            .filter(|entry| entry.llvm_target_bound.is_some())
            .flat_map(|entry| &entry.assumptions)
        {
            if !notes.iter().any(|note| note == assumption) {
                notes.push(assumption.clone());
            }
        }
    }

    if notes.is_empty() {
        return;
    }
    // Separates the notes from the results above -- when there are results.
    // An answer whose entire output is its footer must not open with a blank
    // line.
    if !out.is_empty() {
        out.push('\n');
    }
    let prefix = paint(NOTE, color, Paint::Note);
    for note in notes {
        out.push_str(&format!("{prefix} {note}\n"));
    }
}

/// The envelope's four context blocks, including the counts the adaptive
/// footer omits when they are zero: `scope` with the catalog it was quoted
/// from, the `analysis` breakdown with per-module detail, every
/// `uncertainty` scalar with the binding frontier, and `provenance`.
/// Printed only in [`TextMode::Full`], after the footer.
///
/// Not every field of each block: the selection filters, per-module hashes,
/// triples and compilers, and the identity of each call site stay in
/// `--json`, which is the interface for reading the whole envelope.
fn full_sections(result: &QueryResult, color: Color, out: &mut String) {
    let scope = &result.scope;
    out.push_str(&format!("\n{}\n", paint("scope", color, Paint::Heading)));
    out.push_str(&format!(
        "  entries: {} selected of {}\n",
        scope.selected_entries, scope.total_entries
    ));
    // What a scope claim is worth: `None` is "the catalog did not say", which
    // is not the same as "no".
    out.push_str(&format!(
        "  whole_program_complete: {}\n",
        match scope.whole_program_complete {
            Some(complete) => complete.to_string(),
            None => "unstated".to_string(),
        }
    ));
    for limitation in &scope.limitations {
        out.push_str(&format!("  limitation: {limitation}\n"));
    }

    let analysis = &result.analysis;
    out.push_str(&format!("\n{}\n", paint("analysis", color, Paint::Heading)));
    out.push_str(&format!(
        "  verified: {}  analyzed: {}  changed: {}  missing: {}\n",
        analysis.verified, analysis.analyzed, analysis.changed, analysis.missing
    ));
    out.push_str(&format!(
        "  failed: {}  unsupported: {}  not_built: {}\n",
        analysis.failed, analysis.unsupported, analysis.not_built
    ));
    // `ir_stage` beside `debug_info`, never aggregated: `Analysis::modules`
    // carries both per module because one summary flag would misrepresent a
    // mixed catalog, and printing only one half does the same.
    for module in &analysis.modules {
        out.push_str(&format!(
            "  {} {:?} ir_stage: {:?} debug_info: {:?}\n",
            module.id, module.status, module.ir_stage, module.debug_info
        ));
    }

    let uncertainty = &result.uncertainty;
    out.push_str(&format!(
        "\n{}\n",
        paint("uncertainty", color, Paint::Heading)
    ));
    out.push_str(&format!(
        "  indirect_call_sites: {}\n  sites_with_llvm_target_bound: {}\n",
        uncertainty.indirect_call_sites, uncertainty.sites_with_llvm_target_bound
    ));
    out.push_str(&format!(
        "  functions_without_location: {}\n  locations_from_modified_sources: {}\n",
        uncertainty.functions_without_location, uncertainty.locations_from_modified_sources
    ));
    out.push_str(&format!(
        "  ambiguous_bindings: {}\n  conditional_path_steps: {}\n",
        uncertainty.ambiguous_bindings, uncertainty.conditional_path_steps
    ));
    // Every ambiguous binding `ambiguous_bindings` only counts: for `reach`
    // the ones that walk actually reached, for every other query the full
    // program-wide set. `ambiguous_bindings` says how many; this says which,
    // with the same three facts `PathStep::Binding` shows in the results
    // above, so the two read consistently.
    let full = Ctx {
        mode: TextMode::Full,
        color,
    };
    for binding in &uncertainty.frontier {
        let tint = match binding.status {
            BindingStatus::Unique => Paint::Resolved,
            BindingStatus::Ambiguous => Paint::Uncertain,
            BindingStatus::Unbound => Paint::Absent,
        };
        out.push_str(&format!(
            "  frontier: {}  ({}, {} candidate(s))\n",
            name(&result.symbols, &binding.symbol, full),
            paint(&format!("{:?}", binding.status), color, tint),
            binding.candidates.len()
        ));
    }

    let provenance = &result.provenance;
    out.push_str(&format!(
        "\n{}\n",
        paint("provenance", color, Paint::Heading)
    ));
    // The catalog the answer came from, quoted rather than reconstructed.
    // `scope` is only meaningful next to what it was quoted from.
    let origin = &provenance.catalog_origin;
    out.push_str(&format!(
        "  catalog: {} {}\n",
        origin.kind,
        origin.input.display()
    ));
    if let Some(sha256) = &origin.sha256 {
        out.push_str(&format!("  catalog_sha256: {sha256}\n"));
    }
    out.push_str(&format!("  llvm: {}\n", provenance.llvm_version));
    out.push_str(&format!(
        "  rllvm-query: {}\n",
        provenance.rllvm_query_version
    ));
}

/// Renders one answer. Infallible: every field it reads is already owned by
/// the result.
pub fn render(result: &QueryResult, mode: TextMode, color: Color) -> String {
    let ctx = Ctx { mode, color };
    let mut out = String::new();
    render_results(result, ctx, &mut out);
    footer(result, color, &mut out);
    if mode == TextMode::Full {
        full_sections(result, color, &mut out);
    }
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
                call_sites(symbols, &entry.call_sites, CALL_SITE_INDENT, ctx, out);
            }
        }
        QueryResults::Callers(entries) => {
            for entry in entries {
                out.push_str(&format!("{}\n", name(symbols, &entry.function.symbol, ctx)));
                call_sites(symbols, &entry.call_sites, CALL_SITE_INDENT, ctx, out);
            }
        }
        QueryResults::Callees(sites) => call_sites(symbols, sites, NO_INDENT, ctx, out),
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
            // `None` is no path; `Some(vec![])` is a trivial found path with
            // no steps to print. Both print nothing here, and the footer
            // tells them apart: rule 1 for the absence, `ZERO_STEP_REACH`
            // for the trivial find.
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
                        // The step kind takes the step's own certainty, as
                        // `call` (always resolved) and `bounded-indirect`
                        // (always conditional) do. A binding step has no
                        // fixed certainty, so it borrows its status's:
                        // `Paint::Location` here meant "a file:line" in
                        // every other row and nothing at all in this one.
                        out.push_str(&format!(
                            "{}           {}  ({}, {} candidate(s))\n",
                            paint("binding", ctx.color, tint),
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
        facts::{FunctionFact, Linkage, ModuleAnalysis, ModuleReport},
        index::{Direction, Session},
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
        // program-wide, separated from the results by a blank line. Splitting
        // on that blank line, rather than `starts_with`, keeps the "one
        // line" guarantee: a duplicated or extra definition would still pass
        // `starts_with` but fails this.
        assert_eq!(text.split("\n\n").next().unwrap(), "<no location>  main");
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
    fn an_externals_answer_with_no_unbound_symbols_explains_itself_in_the_footer() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Externals).unwrap();
        // This fixture binds no symbols at all, so no row prints: the
        // footer, not a row, says so.
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("every symbol in scope bound to a definition"));
        // And it says so on the first line: the blank line that separates
        // notes from results has no results to separate them from here.
        assert!(text.starts_with(NOTE), "leading blank line: {text:?}");
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
        // `session_from` gives every call site `location: None`, so
        // `text.contains("a")` used to pass on the footer's own
        // "2 function(s) without a source location" alone, with no call-site
        // row at all. Give the site a real location, so only an actual
        // rendered caller row -- not the footer -- can satisfy this.
        let caller = function("m", "a", true, Linkage::Internal);
        let callee = function("m", "b", true, Linkage::Internal);
        let mut site = direct_call(&caller, &callee, 0);
        site.location = Some(source_location("caller.c", 7));
        let session = Session::new(facts(vec![caller, callee], vec![site]), Vec::new());
        let result = run(&session, &Query::Callers { name: "b".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("caller.c:7"), "got: {text}");
    }

    #[test]
    fn callees_names_the_target_kind() {
        // `text.contains("indirect")` used to pass on the footer's own
        // "N indirect call site(s)" line alone, with the whole `Callees` arm
        // of `render_results` deleted -- the same defect
        // `callers_reports_each_call_site_with_its_location` above fixed.
        // Only a rendered row carries the kind and the signature on one
        // line, so only a rendered row can satisfy this.
        let session = session_with_bounded_indirect();
        let result = run(&session, &Query::Callees { name: "a".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.lines()
                .any(|line| line.contains("indirect") && line.contains("i32 (i32, i32)")),
            "no rendered call-site row: {text}"
        );
    }

    #[test]
    fn callees_rows_start_at_column_zero_and_callers_rows_nest() {
        // `call_sites` nests a row under the parent line naming the function
        // that owns it. `callees` prints no parent line, so an indented row
        // there hangs under nothing while every other query's results start
        // at column 0.
        let session = session_with_bounded_indirect();
        let text_of = |query| {
            render(
                &run(&session, &query).unwrap(),
                TextMode::Adaptive,
                Color::Never,
            )
        };

        let callees = text_of(Query::Callees { name: "a".into() });
        let row = callees.lines().next().unwrap_or_default();
        assert!(row.contains("indirect"), "no row rendered: {callees}");
        assert!(!row.starts_with(' '), "callees row is indented: {row:?}");

        // The nesting is still right where there is a parent to nest under.
        let callers = text_of(Query::Callers {
            name: "target".into(),
        });
        let mut lines = callers.lines();
        assert_eq!(lines.next(), Some("a"), "got: {callers}");
        let nested = lines.next().unwrap_or_default();
        assert!(
            nested.starts_with("    ") && nested.contains("indirect"),
            "caller call site must stay nested: {callers}"
        );
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
    fn a_zero_step_path_says_the_origin_is_already_the_destination() {
        // `reach a a` finds `a` immediately: a found path with no steps to
        // print. With nothing in the footer for it the command emits zero
        // bytes, which a reader can only read as "unreachable" -- the
        // opposite of the answer.
        let session = session_from(&[("a", "b")]);
        let result = run(
            &session,
            &Query::Reach {
                from: "a".into(),
                to: "a".into(),
            },
        )
        .unwrap();
        assert!(
            matches!(&result.results, QueryResults::Reach(Some(steps)) if steps.is_empty()),
            "fixture changed: expected a found path with no steps"
        );
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("reached in zero steps"), "got: {text:?}");
        assert!(
            !text.contains("not proof of unreachability"),
            "a found path must not read as an absent one: {text:?}"
        );
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
        // Nothing was bounded here, so there is no bound to qualify.
        assert!(
            !text.contains("only within the captured scope"),
            "got: {text}"
        );
    }

    #[test]
    fn an_indirect_bound_says_it_holds_only_in_the_captured_scope() {
        // `IndirectTargetsResult::assumptions` tells a JSON reader that the
        // bound is scope-local -- `dlopen` and a callback registered outside
        // the capture both escape it. Text printed the bound with no such
        // qualification, claiming more than the answer knows.
        let session = session_with_bounded_indirect();
        let result = run(
            &session,
            &Query::IndirectTargets {
                at: "t.c:9".into(),
                heuristics: false,
            },
        )
        .unwrap();
        assert!(
            matches!(&result.results, QueryResults::IndirectTargets(entries)
                if entries.iter().any(|entry| entry.llvm_target_bound.is_some())),
            "fixture changed: expected a bounded site at t.c:9"
        );
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("bound:"), "got: {text}");
        assert!(
            text.contains("only within the captured scope"),
            "a text reader must be told the bound is scope-local: {text}"
        );
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
    fn the_missing_location_note_follows_the_answer_not_the_program() {
        // `functions_without_location` counts the whole session, and every
        // `extern` declaration lands in it, so on a real multi-file program
        // it is non-zero for every query. Printing it unconditionally makes
        // it a constant the reader skips past -- together with the notes
        // that do bear on the answer.
        let located = FunctionFact {
            location: Some(source_location("a.c", 1)),
            ..function("m", "located", true, Linkage::Internal)
        };
        let unlocated = function("m", "unlocated", true, Linkage::Internal);
        let session = Session::new(facts(vec![located, unlocated], vec![]), Vec::new());
        let defs_of = |name: &str| {
            let result = run(&session, &Query::Defs { name: name.into() }).unwrap();
            assert!(result.uncertainty.functions_without_location > 0, "fixture");
            render(&result, TextMode::Adaptive, Color::Never)
        };

        let quiet = defs_of("located");
        assert!(
            !quiet.contains("without a source location"),
            "an answer that shows no missing location must not carry the note: {quiet:?}"
        );

        let loud = defs_of("unlocated");
        assert!(loud.contains(NO_LOCATION), "got: {loud:?}");
        assert!(loud.contains("without a source location"), "got: {loud:?}");
    }

    #[test]
    fn the_indirect_call_note_follows_the_answer_not_the_program() {
        // Same rule for `indirect_call_sites`: one indirect call anywhere in
        // the program must not annotate every `defs` answer.
        let session = session_with_bounded_indirect();
        let text_of = |query| {
            let result = run(&session, &query).unwrap();
            assert!(result.uncertainty.indirect_call_sites > 0, "fixture");
            render(&result, TextMode::Adaptive, Color::Never)
        };

        let callees = text_of(Query::Callees { name: "a".into() });
        assert!(callees.contains("indirect call site"), "got: {callees:?}");

        let defs = text_of(Query::Defs {
            name: "target".into(),
        });
        assert!(
            !defs.contains("indirect call site"),
            "an answer with no indirect site must not carry the note: {defs:?}"
        );
    }

    #[test]
    fn a_walk_over_call_edges_carries_the_indirect_note_it_shows_no_row_for() {
        // `Session::callers` resolves a direct call and a CVP-bounded one and
        // nothing else, so an unbounded indirect site is precisely the edge a
        // `callers` or `closure` answer is missing. Gating the note on
        // whether the answer *displays* an indirect row silences it exactly
        // there: `callers` here shows one direct row, and `closure` prints
        // bare names and can never show a call site at all.
        let session = session_with_indirect_gap();
        let text_of = |query| {
            let result = run(&session, &query).unwrap();
            assert!(
                result.uncertainty.indirect_call_sites > 0
                    && result.uncertainty.sites_with_llvm_target_bound == 0,
                "fixture changed: expected an unbounded indirect site"
            );
            assert!(!result.results.is_empty(), "fixture changed: empty answer");
            render(&result, TextMode::Adaptive, Color::Never)
        };

        let callers = text_of(Query::Callers { name: "b".into() });
        assert!(
            !callers
                .lines()
                .any(|line| !line.starts_with(NOTE) && line.contains("indirect")),
            "fixture changed: expected only direct rows: {callers:?}"
        );
        assert!(callers.contains("indirect call site"), "got: {callers:?}");

        let closure = text_of(Query::Closure {
            name: "a".into(),
            direction: Direction::Out,
        });
        assert!(closure.contains("indirect call site"), "got: {closure:?}");
    }

    #[test]
    fn an_answer_that_walks_no_call_edges_still_omits_the_indirect_note() {
        // The other half: `defs` and `externals` are not walks over call
        // edges, so a program-wide indirect count cannot change what they
        // mean and must not annotate every answer of a program that has one.
        let session = session_with_indirect_gap();
        let text_of = |query| {
            let result = run(&session, &query).unwrap();
            assert!(result.uncertainty.indirect_call_sites > 0, "fixture");
            render(&result, TextMode::Adaptive, Color::Never)
        };

        let defs = text_of(Query::Defs { name: "a".into() });
        assert!(!defs.contains("indirect call site"), "got: {defs:?}");

        let externals = text_of(Query::Externals);
        assert!(
            !externals.contains("indirect call site"),
            "got: {externals:?}"
        );
    }

    #[test]
    fn a_clean_answer_prints_no_footer() {
        // `session_from` leaves every function's `location` unset, which
        // would itself trip the footer's "functions without a location"
        // count; a genuinely clean answer needs functions that have one.
        let caller = FunctionFact {
            location: Some(source_location("a.c", 1)),
            ..function("m", "a", true, Linkage::Internal)
        };
        let callee = FunctionFact {
            location: Some(source_location("a.c", 2)),
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
        // `facts_with_one_failed_module` alone has no functions, so
        // `Externals` used to return empty and rule 1 fired instead of rule
        // 3 -- the "results are present" half of this test's name was never
        // exercised. Add a function under the module that did parse, and
        // query it directly, so a result row and the failed-module note both
        // appear in the same answer.
        let mut facts = facts_with_one_failed_module();
        facts
            .functions
            .push(function("a", "present", true, Linkage::Internal));
        let result = run(
            &Session::new(facts, vec![]),
            &Query::Defs {
                name: "present".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("<no location>  present"), "got: {text}");
        assert!(text.contains("failed"), "got: {text}");
    }

    #[test]
    fn several_unread_categories_sum_to_the_total_modules_not_read() {
        // 3 failed + 2 missing must read "5 of N", not "2 of N": `unread`
        // has one entry per non-zero *category*, and summing the counts
        // catches a regression to `unread.len()` that this crate's earlier
        // one-category, one-module fixtures could not.
        let mut facts = facts(vec![], vec![]);
        facts.scope.total_entries = 5;
        facts.scope.selected_entries = 5;
        facts.modules = vec![
            report("f1", ModuleAnalysis::Failed),
            report("f2", ModuleAnalysis::Failed),
            report("f3", ModuleAnalysis::Failed),
            report("m1", ModuleAnalysis::Missing),
            report("m2", ModuleAnalysis::Missing),
        ];
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains("5 of 5 modules were not analysed: 3 failed, 2 missing"),
            "got: {text}"
        );
    }

    #[test]
    fn a_verified_but_unextracted_module_is_reported() {
        // `Analysis::verified` is non-zero only for a module the loader read
        // and hashed but extraction never touched; its own doc warns that a
        // status with no count would let it disappear from the summary. The
        // footer must not reintroduce that.
        let facts = facts_with_one_verified_module();
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("1 verified"), "got: {text}");
    }

    #[test]
    fn full_mode_prints_scope_analysis_and_provenance() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Callers { name: "b".into() }).unwrap();
        let text = render(&result, TextMode::Full, Color::Never);
        for heading in ["scope", "analysis", "uncertainty", "provenance"] {
            assert!(text.contains(heading), "missing {heading}: {text}");
        }
    }

    #[test]
    fn full_mode_prints_the_catalog_origin_scope_completeness_and_ir_stage() {
        // Three fields a text reader had no way to see. `scope` means
        // little without the catalog it was quoted from;
        // `whole_program_complete` is what decides whether a scope claim is
        // worth anything; and `Analysis::modules` carries `ir_stage` beside
        // `debug_info` exactly because one aggregate would misrepresent a
        // mixed catalog -- printing one half repeats that mistake.
        let mut facts = facts(vec![], vec![]);
        facts.scope.whole_program_complete = Some(false);
        facts.modules = vec![ModuleReport {
            ir_stage: Some("linked".into()),
            ..report("m", ModuleAnalysis::Analyzed)
        }];
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        let text = render(&result, TextMode::Full, Color::Never);
        assert!(
            text.contains("whole_program_complete: false"),
            "got: {text}"
        );
        assert!(text.contains(r#"ir_stage: Some("linked")"#), "got: {text}");
        assert!(text.contains("catalog: test"), "got: {text}");
    }

    #[test]
    fn full_mode_prints_zero_counts_the_footer_omits() {
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Callers { name: "b".into() }).unwrap();
        assert!(render(&result, TextMode::Full, Color::Never).contains("ambiguous_bindings: 0"));
    }

    #[test]
    fn full_mode_prints_the_frontier_of_ambiguous_bindings() {
        let session = session_with_ambiguous_bindings();
        let result = run(&session, &Query::Externals).unwrap();
        assert!(
            !result.uncertainty.frontier.is_empty(),
            "fixture changed: expected a non-empty frontier"
        );
        let text = render(&result, TextMode::Full, Color::Never);
        assert!(text.contains("frontier: target"), "got: {text}");
        assert!(text.contains("Ambiguous"), "got: {text}");
        assert!(text.contains("2 candidate(s))"), "got: {text}");
    }

    #[test]
    fn a_name_that_matched_nothing_says_so_once() {
        // Rule 1's catch-all reading of an empty `defs` and rule 2's
        // "'absent' matched nothing" are the same caveat; the second names
        // the request, so it is the one that survives.
        let session = session_from(&[("a", "b")]);
        let result = run(
            &session,
            &Query::Defs {
                name: "absent".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("'absent' matched nothing"), "got: {text}");
        assert_eq!(
            text.lines().filter(|line| line.starts_with(NOTE)).count(),
            1,
            "one caveat, stated once: {text}"
        );
    }

    #[test]
    fn a_loosely_matched_name_says_how_many_symbols_it_gathered() {
        // A fuzzy hit can gather unrelated functions that merely share an
        // identifier, so an answer built from one must not read like an
        // exact hit. `twice` finds both instantiations and `ns::twice`.
        let session = session_with_cxx_symbols();
        let result = run(
            &session,
            &Query::Defs {
                name: "twice".into(),
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains("'twice' matched loosely, gathering 3 symbol(s)"),
            "got: {text}"
        );
    }

    #[test]
    fn an_empty_callers_answer_says_no_call_was_found() {
        // `a` calls `b`, so `a` itself has no callers in this fixture.
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Callers { name: "a".into() }).unwrap();
        assert!(result.results.is_empty(), "fixture changed");
        assert!(
            render(&result, TextMode::Adaptive, Color::Never)
                .contains("no call to the target was found in the selected scope")
        );
    }

    #[test]
    fn an_empty_uses_answer_says_no_non_call_use_was_found() {
        // `session_from` records no `UseFact`s at all, so any resolved name
        // answers empty here.
        let session = session_from(&[("a", "b")]);
        let result = run(&session, &Query::Uses { name: "a".into() }).unwrap();
        assert!(result.results.is_empty(), "fixture changed");
        assert!(
            render(&result, TextMode::Adaptive, Color::Never)
                .contains("no non-call use of the target was found in the selected scope")
        );
    }

    #[test]
    fn an_empty_closure_answer_says_no_functions_were_found() {
        // `b` is only ever called, never a caller itself, so its outward
        // closure is empty.
        let session = session_from(&[("a", "b")]);
        let result = run(
            &session,
            &Query::Closure {
                name: "b".into(),
                direction: Direction::Out,
            },
        )
        .unwrap();
        assert!(result.results.is_empty(), "fixture changed");
        assert!(
            render(&result, TextMode::Adaptive, Color::Never)
                .contains("no functions were found in the selected scope for that direction")
        );
    }
}
