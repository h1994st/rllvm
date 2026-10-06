//! Human-readable rendering of a [`QueryResult`].
//!
//! Returns a `String` rather than writing to stdout so the renderers are
//! ordinary unit tests rather than capture harnesses. The JSON envelope is
//! untouched: `--json` still serializes [`QueryResult`] directly, and MCP
//! never reaches this module at all.

use std::collections::BTreeMap;

use owo_colors::OwoColorize;

use crate::{
    EdgeKey, PathStep, QueryResult, QueryResults,
    bind::BindingStatus,
    facts::{CallSiteFact, CallTarget, FieldEvidence, FunctionId, SourceLocation},
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

/// Bytes per MiB. Cache sizes below a GiB print in MiB labelled "MB",
/// matching `query_cache_warn_mb`.
pub(crate) const MIB: u64 = 1024 * 1024;

/// Bytes per GiB: the threshold at which [`human_bytes`] switches from MB to
/// GB.
const GIB: u64 = 1024 * MIB;

/// `3774873` → `3.6 MB`; `1363148800` → `1.3 GB`.
///
/// `pub` so the `rllvm-query` binary can format `cache clear`'s byte count
/// with it; not part of the crate's documented API.
#[doc(hidden)]
pub fn human_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    }
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

/// The name a unit-variant enum carries into the JSON envelope, not the
/// [`std::fmt::Debug`] one. `{:?}` prints the Rust variant identifier, but
/// `UseKind`, `BindingStatus` and `ModuleAnalysis` all serialize under
/// `#[serde(rename_all = "snake_case")]` for `--json`; formatting them with
/// `{:?}` in text gave the same fact two spellings depending on which output
/// was asked for, and let renaming a Rust variant change the CLI's text
/// silently, with nothing pinning it to the envelope. A unit variant cannot
/// realistically fail to serialize to a string, but this still returns a
/// visibly-wrong marker rather than an empty string if it somehow does, so a
/// reader sees a bug rather than a blank field.
fn serde_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(spelling)) => spelling,
        Ok(other) => format!("<unrenderable:{other}>"),
        Err(err) => format!("<unrenderable:{err}>"),
    }
}

/// Printed for an absent value in [`option_or_unknown`], following the same
/// bracket convention as [`NO_LOCATION`] so every "we don't know" in this
/// output reads the same way.
const UNKNOWN: &str = "<unknown>";

/// The contained value of an `Option`, not the `Option` wrapper. `{:?}` on an
/// `Option<T>` prints Rust's own debug syntax -- the `Some(...)` wrapper and,
/// for a string, its quotes -- which is an implementation detail of how the
/// fact is stored, not a fact about the program, and it diverges from what
/// `--json` reports for the same field: text would read `Some("O2")` next to
/// the envelope's plain `"O2"`. Prints the bare value when present and
/// [`UNKNOWN`] when absent, never `None` or an empty string, either of which
/// would read as a missing field rather than one the catalog never recorded.
fn option_or_unknown<T: std::fmt::Display>(value: &Option<T>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => UNKNOWN.to_string(),
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
/// `1 site`, `2 sites`.
fn count(n: usize, noun: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {noun}{plural}")
}

fn symbol_list(symbols: &BTreeMap<String, String>, ids: &[FunctionId], ctx: Ctx) -> String {
    ids.iter()
        .map(|id| name(symbols, &id.symbol, ctx))
        .collect::<Vec<_>>()
        .join(", ")
}

/// ` via ops@8 (on_event)`: the record field a site loads from or a use
/// goes into, after `word`, with the member name when debug info gave one.
/// Empty when the IR proved no field.
fn field_note(word: &str, evidence: Option<&FieldEvidence>, ctx: Ctx) -> String {
    let Some(evidence) = evidence else {
        return String::new();
    };
    let name = evidence
        .name
        .as_ref()
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    format!(
        " {word} {}{name}",
        paint(&evidence.field.to_string(), ctx.color, Paint::Symbol)
    )
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
            via_field,
        } => {
            let kind = paint("indirect", ctx.color, Paint::Uncertain);
            let signature = paint(signature, ctx.color, Paint::Muted);
            let field = field_note("via", via_field.as_ref(), ctx);
            match llvm_target_bound {
                Some(bound) => format!(
                    "{kind}  {signature}  {} {}{field}",
                    paint("bound:", ctx.color, Paint::Resolved),
                    symbol_list(symbols, bound, ctx)
                ),
                None => format!(
                    "{kind}  {signature}  {}{field}",
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
        QueryResults::Defs(entries) | QueryResults::FfiExports(entries) => {
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
            // Only a use inside a function lacks a location for want of debug
            // info; a global's initializer has none to lack.
            shown.missing_location = uses
                .iter()
                .any(|use_fact| use_fact.in_function.is_some() && use_fact.location.is_none());
        }
        QueryResults::Reach(path) => {
            // Only an agent step prints a location. Its site and a bounded
            // one are the path's indirect steps.
            for step in path.iter().flatten() {
                match step {
                    PathStep::BoundedIndirect { .. } => shown.indirect_call_site = true,
                    PathStep::Agent { location, .. } => {
                        shown.indirect_call_site = true;
                        shown.missing_location |= location.is_none();
                    }
                    PathStep::Call(_) | PathStep::Binding(_) | PathStep::Alias { .. } => {}
                }
            }
        }
        QueryResults::IndirectTargets(entries) => {
            shown.missing_location = entries.iter().any(|entry| entry.location.is_none());
            // Every row of this answer is an indirect call site by
            // construction; the word itself is in the query's name rather
            // than the row.
            shown.indirect_call_site = !entries.is_empty();
        }
        QueryResults::ResolutionCandidates(groups) => {
            shown.missing_location = groups
                .iter()
                .any(|group| group.sites.iter().any(|site| site.location.is_none()));
            // Every group is made of unresolved indirect call sites.
            shown.indirect_call_site = !groups.is_empty();
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
        | QueryResults::FfiExports(_)
        | QueryResults::IndirectTargets(_)
        | QueryResults::ResolutionCandidates(_) => false,
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
        QueryResults::FfiExports(_) => "no function attributed to Rust exports an unmangled symbol",
        // `Callers`, `Uses` and `Closure` all take a name that can resolve
        // perfectly well and still return no results, so the
        // `MATCHED_NOTHING` reading below would be false: rule 2 stays
        // silent in that case because the name genuinely did match.
        QueryResults::Callers(_) => "no call to the target was found in the selected scope",
        QueryResults::Uses(_) => "no non-call use of the target was found in the selected scope",
        QueryResults::Closure(_) => {
            "no functions were found in the selected scope for that direction"
        }
        QueryResults::ResolutionCandidates(_) => {
            "no unresolved indirect call site in the selected scope"
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
        // The count of modules not analyzed, not the count of non-zero
        // categories: 3 failed + 2 missing is 5 unread modules, and
        // `unread.len()` (2, one per category) would understate that.
        let unread_modules: usize = module_counts.iter().map(|(_, count)| count).sum();
        notes.push(format!(
            "{} of {} modules were not analyzed: {}",
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
    if let Some(overlay) = &uncertainty.overlay {
        notes.push(format!(
            "overlay edges cover {} of {} unresolved indirect site(s)",
            overlay.covered_sites, overlay.unresolved_sites
        ));
    }
    if let Some(unknown) = uncertainty.functions_of_unknown_language
        && unknown > 0
    {
        notes.push(format!(
            "{unknown} unmangled definition(s) could not be attributed to a language and were not searched: they have no debug info, and their module's producers are mixed or absent"
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

    // Rule 7: a facts cache past its threshold, the one cache fact a reader
    // must act on.
    if let Some(cache) = analysis.cache.as_ref().filter(|cache| cache.over_threshold) {
        notes.push(format!(
            "facts cache is {}, over query_cache_warn_mb ({} MB); prune with rllvm-query cache clear",
            human_bytes(cache.disk_bytes),
            cache.warn_bytes / MIB
        ));
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
    if let Some(cache) = &analysis.cache {
        out.push_str(&format!(
            "  cache: {} hit, {} miss, {} written, {}\n",
            cache.hits,
            cache.misses,
            cache.written,
            human_bytes(cache.disk_bytes)
        ));
    }
    // `ir_stage` beside `debug_info`, never aggregated: `Analysis::modules`
    // carries both per module because one summary flag would misrepresent a
    // mixed catalog, and printing only one half does the same.
    for module in &analysis.modules {
        out.push_str(&format!(
            "  {} {} ir_stage: {} debug_info: {}\n",
            module.id,
            serde_name(&module.status),
            option_or_unknown(&module.ir_stage),
            option_or_unknown(&module.debug_info)
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
    if let Some(unknown) = uncertainty.functions_of_unknown_language {
        out.push_str(&format!("  functions_of_unknown_language: {unknown}\n"));
    }
    // Only for a walk that included the overlay: without it they say nothing.
    if let Some(overlay) = &uncertainty.overlay {
        out.push_str(&format!(
            "  agent_path_steps: {}\n  overlay: {} of {} unresolved site(s) covered\n",
            uncertainty.agent_path_steps, overlay.covered_sites, overlay.unresolved_sites
        ));
    }
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
            paint(&serde_name(&binding.status), color, tint),
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
    // Last, in every mode, so no reader stops before it.
    let agent_steps = result.uncertainty.agent_path_steps;
    if agent_steps > 0 {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} this path uses {agent_steps} agent edge(s)\n",
            paint("not proven:", color, Paint::Uncertain)
        ));
    }
    out
}

fn render_results(result: &QueryResult, ctx: Ctx, out: &mut String) {
    let symbols = &result.symbols;
    match &result.results {
        QueryResults::Defs(entries) | QueryResults::FfiExports(entries) => {
            for entry in entries {
                out.push_str(&format!(
                    "{}  {}\n",
                    location(entry.location.as_ref(), ctx),
                    name(symbols, &entry.function.symbol, ctx)
                ));
            }
        }
        QueryResults::Closure(functions) => {
            let agent_reached = &result.uncertainty.agent_reached;
            for function in functions.iter().filter(|id| !agent_reached.contains(id)) {
                out.push_str(&format!("{}\n", name(symbols, &function.symbol, ctx)));
            }
            if !agent_reached.is_empty() {
                out.push_str(&format!(
                    "{}\n",
                    paint(
                        "reached only through agent edges:",
                        ctx.color,
                        Paint::Uncertain
                    )
                ));
                for function in agent_reached {
                    out.push_str(&format!("  {}\n", name(symbols, &function.symbol, ctx)));
                }
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
                    paint(&serde_name(&binding.status), ctx.color, tint)
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
                let holder = match (&use_fact.in_function, &use_fact.in_global) {
                    (Some(id), _) => name(symbols, &id.symbol, ctx),
                    (None, Some(global)) => name(symbols, global, ctx),
                    (None, None) => paint("<no function>", ctx.color, Paint::Muted),
                };
                out.push_str(&format!(
                    "{}  in {}  at {}{}\n",
                    paint(&serde_name(&use_fact.kind), ctx.color, Paint::Uncertain),
                    holder,
                    location(use_fact.location.as_ref(), ctx),
                    field_note("into", use_fact.field.as_ref(), ctx)
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
                            paint(&serde_name(&binding.status), ctx.color, tint),
                            binding.candidates.len()
                        ));
                    }
                    PathStep::Alias { alias, target } => out.push_str(&format!(
                        "{}             {}  (of {})\n",
                        paint("alias", ctx.color, Paint::Resolved),
                        name(symbols, &alias.symbol, ctx),
                        name(symbols, &target.symbol, ctx)
                    )),
                    PathStep::Agent {
                        site,
                        chosen,
                        location: at,
                        key,
                        confidence,
                        verdict,
                    } => {
                        let via = match key {
                            EdgeKey::Field { via_field, .. } => format!(
                                " via {}",
                                paint(&via_field.to_string(), ctx.color, Paint::Symbol)
                            ),
                            EdgeKey::Site { .. } => String::new(),
                        };
                        let verdict = verdict.map_or_else(
                            || "unverified".to_string(),
                            |verdict| verdict.to_string(),
                        );
                        out.push_str(&format!(
                            "{}             {} -> {} at {}{via} {}\n",
                            paint("agent", ctx.color, Paint::Uncertain),
                            name(symbols, &site.function.symbol, ctx),
                            name(symbols, &chosen.symbol, ctx),
                            location(at.as_ref(), ctx),
                            paint(
                                &format!("[{confidence}, {verdict}]"),
                                ctx.color,
                                Paint::Uncertain
                            )
                        ));
                    }
                }
            }
        }
        QueryResults::ResolutionCandidates(groups) => {
            for group in groups {
                let subject = match &group.field {
                    Some(field) => format!(
                        "field {field}{}",
                        group
                            .field_name
                            .as_ref()
                            .map(|name| format!(" ({name})"))
                            .unwrap_or_default()
                    ),
                    None => format!("no field, {}", group.signature),
                };
                let hint = if group.single_candidate {
                    format!(" {}", paint("[single]", ctx.color, Paint::Uncertain))
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "{} \u{2014} {}, {}{hint}\n",
                    paint(&subject, ctx.color, Paint::Heading),
                    count(group.sites.len(), "site"),
                    count(group.candidates.len(), "candidate"),
                ));
                for site in &group.sites {
                    out.push_str(&format!(
                        "  site {} {}\n",
                        name(symbols, &site.site.function.symbol, ctx),
                        location(site.location.as_ref(), ctx)
                    ));
                }
                for candidate in &group.candidates {
                    for assignment in &candidate.assignments {
                        let holder = match (&assignment.in_function, &assignment.in_global) {
                            (Some(id), _) => name(symbols, &id.symbol, ctx),
                            (None, Some(global)) => name(symbols, global, ctx),
                            (None, None) => paint("global", ctx.color, Paint::Muted),
                        };
                        out.push_str(&format!(
                            "  candidate {} assigned in {} {}\n",
                            name(symbols, &candidate.function.symbol, ctx),
                            holder,
                            location(assignment.location.as_ref(), ctx)
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
                // The candidates are the signature-compatible subset; the
                // unfiltered inventory is only its denominator.
                if let (Some(inventory), Some(compatible)) =
                    (&entry.address_taken_inventory, &entry.signature_compatible)
                {
                    out.push_str(&format!(
                        "    {}\n",
                        paint(
                            &format!(
                                "address-taken candidates: {} of {} address-taken function(s) match the signature",
                                compatible.len(),
                                inventory.len()
                            ),
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
        CacheReport, Query,
        facts::{
            FieldBasis, FieldEvidence, FieldRef, FunctionFact, Language, Linkage, ModuleAnalysis,
            ModuleReport, UseFact, UseKind,
        },
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
                include_overlay: false,
                min_confidence: None,
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
                include_overlay: false,
                min_confidence: None,
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
                include_overlay: false,
                min_confidence: None,
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("bounded-indirect"), "got: {text}");
    }

    /// The fixture's `dispatch` calls through `ops@8` with no bound, and an
    /// agent says that call takes `h3`.
    fn overlay_answer(query: Query) -> QueryResult {
        use crate::overlay::{Confidence, Overlay, Record, TargetSpec};
        let session = Session::new(facts_with_overlay_sites(), Vec::new());
        let mut overlay = Overlay::empty(&session, None).unwrap();
        overlay
            .record(
                &session,
                vec![Record::Add {
                    via_field: Some(FieldRef {
                        record: "ops".into(),
                        offset: 8,
                    }),
                    site: None,
                    to: TargetSpec::Symbol("h3".into()),
                    confidence: Confidence::High,
                    provenance: vec!["init: o->on_event = h3".into()],
                    source: "agent".into(),
                }],
            )
            .unwrap();
        crate::run_with_overlay(&session, Some(&overlay), &query).unwrap()
    }

    #[test]
    fn an_agent_step_is_labeled_and_the_answer_ends_not_proven() {
        let result = overlay_answer(Query::Reach {
            from: "dispatch".into(),
            to: "h3".into(),
            include_overlay: true,
            min_confidence: None,
        });
        for mode in [TextMode::Adaptive, TextMode::Full] {
            let text = render(&result, mode, Color::Never);
            assert!(
                text.contains(
                    "agent             dispatch -> h3 at t.c:4 via ops@8 [high, unverified]\n"
                ),
                "got: {text}"
            );
            assert!(
                text.ends_with("\nnot proven: this path uses 1 agent edge(s)\n"),
                "got: {text}"
            );
            assert!(
                text.contains("overlay edges cover 1 of 2 unresolved indirect site(s)"),
                "got: {text}"
            );
        }
    }

    #[test]
    fn a_closure_lists_what_only_agent_edges_reach_apart() {
        let result = overlay_answer(Query::Closure {
            name: "h3".into(),
            direction: Direction::In,
            include_overlay: true,
            min_confidence: None,
        });
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.starts_with("init\nreached only through agent edges:\n  dispatch\n"),
            "got: {text}"
        );
        assert!(
            !text.contains("not proven"),
            "a closure has no path: {text}"
        );
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

    /// `ops@8` dispatched twice, stored into by `init` (a function) and by
    /// the global `table`; `on_event` is named only by the stores.
    fn candidates_text(second_candidate: bool) -> String {
        let caller = function("m", "caller", true, Linkage::Internal);
        let h1 = function("m", "h1", true, Linkage::Internal);
        let h3 = function("m", "h3", true, Linkage::Internal);
        let field = FieldRef {
            record: "ops".into(),
            offset: 8,
        };
        let evidence = FieldEvidence {
            field: field.clone(),
            basis: FieldBasis::StructGep,
            name: Some("on_event".into()),
        };
        let mut base = facts(
            vec![caller.clone(), h1.clone(), h3.clone()],
            vec![
                indirect_call_through(&caller, 1, Some(field.clone())),
                indirect_call_through(&caller, 2, Some(field)),
            ],
        );
        base.uses = vec![UseFact {
            used: h1.id.clone(),
            in_function: None,
            in_global: Some("table".into()),
            location: Some(source_location("t.c", 9)),
            kind: UseKind::GlobalInitializer,
            field: Some(evidence.clone()),
        }];
        if second_candidate {
            base.uses.push(UseFact {
                used: h3.id.clone(),
                in_function: Some(caller.id.clone()),
                in_global: None,
                location: Some(source_location("t.c", 12)),
                kind: UseKind::StoredToMemory,
                field: Some(evidence),
            });
        }
        let result = run(
            &Session::new(base, Vec::new()),
            &Query::ResolutionCandidates,
        )
        .unwrap();
        render(&result, TextMode::Adaptive, Color::Never)
    }

    #[test]
    fn resolution_candidates_header_counts_sites_and_candidates() {
        let text = candidates_text(true);
        assert!(
            text.contains("field ops@8 (on_event) \u{2014} 2 sites, 2 candidates\n"),
            "got: {text}"
        );
        assert!(!text.contains("[single]"), "got: {text}");
        assert!(text.contains("  site caller t.c:1\n"), "got: {text}");
    }

    #[test]
    fn a_single_candidate_header_is_singular_and_marked() {
        let text = candidates_text(false);
        assert!(
            text.contains("field ops@8 (on_event) \u{2014} 2 sites, 1 candidate [single]\n"),
            "got: {text}"
        );
    }

    #[test]
    fn a_candidate_line_names_a_function_or_global_holder() {
        let text = candidates_text(true);
        assert!(
            text.contains("  candidate h1 assigned in table t.c:9\n"),
            "got: {text}"
        );
        assert!(
            text.contains("  candidate h3 assigned in caller t.c:12\n"),
            "got: {text}"
        );
    }

    #[test]
    fn a_group_without_a_field_is_headed_by_its_signature() {
        let session = session_with_address_taken_function();
        let result = run(&session, &Query::ResolutionCandidates).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains("no field, i32 (i32, i32) \u{2014} 1 site, 1 candidate [single]\n"),
            "got: {text}"
        );
    }

    #[test]
    fn heuristic_candidates_count_only_signature_matches() {
        // `add` and `log` both have their address taken; only `add` matches
        // the site's signature. The note calls the candidates
        // signature-matched, so the count must be too.
        let session = session_with_address_taken_function();
        let result = run(
            &session,
            &Query::IndirectTargets {
                at: "t.c:4".into(),
                heuristics: true,
            },
        )
        .unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains("address-taken candidates: 1 of 2 address-taken function(s) match"),
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
                include_overlay: false,
                min_confidence: None,
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
    fn a_use_in_a_global_initializer_does_not_blame_functions_for_its_location() {
        let run_fn = function("m", "run", true, Linkage::Internal);
        let mut base = facts(vec![run_fn.clone()], vec![]);
        base.uses = vec![UseFact {
            used: run_fn.id,
            in_function: None,
            in_global: Some("table".into()),
            location: None,
            kind: UseKind::GlobalInitializer,
            field: None,
        }];
        let session = Session::new(base, Vec::new());
        let result = run(&session, &Query::Uses { name: "run".into() }).unwrap();
        assert!(result.uncertainty.functions_without_location > 0, "fixture");
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("in table"), "got: {text:?}");
        assert!(
            !text.contains("without a source location"),
            "a global's use has no function to lack a location: {text:?}"
        );

        // A use inside a function without debug info still carries the note.
        let session = session_with_address_taken_function();
        let result = run(&session, &Query::Uses { name: "add".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(text.contains("without a source location"), "got: {text:?}");
    }

    fn ops_field(name: Option<&str>) -> FieldEvidence {
        FieldEvidence {
            field: FieldRef {
                record: "ops".into(),
                offset: 8,
            },
            basis: FieldBasis::Tbaa,
            name: name.map(str::to_string),
        }
    }

    #[test]
    fn an_indirect_site_names_the_field_it_loads_from() {
        let caller = function("m", "caller", true, Linkage::Internal);
        let callees = |name: Option<&str>| {
            let mut site = indirect_call(&caller, 0, None, None);
            if let CallTarget::Indirect { via_field, .. } = &mut site.target {
                *via_field = Some(ops_field(name));
            }
            let session = Session::new(facts(vec![caller.clone()], vec![site]), Vec::new());
            let result = run(
                &session,
                &Query::Callees {
                    name: "caller".into(),
                },
            )
            .unwrap();
            render(&result, TextMode::Adaptive, Color::Never)
        };

        let named = callees(Some("on_event"));
        assert!(
            named.contains("unresolved via ops@8 (on_event)"),
            "got: {named:?}"
        );
        let unnamed = callees(None);
        assert!(
            unnamed.contains("unresolved via ops@8\n"),
            "got: {unnamed:?}"
        );
    }

    #[test]
    fn a_stored_use_names_the_field_it_goes_into() {
        let add = function("m", "add", true, Linkage::Internal);
        let init = function("m", "init", true, Linkage::Internal);
        let mut base = facts(vec![add.clone(), init.clone()], vec![]);
        base.uses = vec![UseFact {
            used: add.id,
            in_function: Some(init.id),
            in_global: None,
            location: Some(source_location("t.c", 3)),
            kind: UseKind::StoredToMemory,
            field: Some(ops_field(Some("on_event"))),
        }];
        let session = Session::new(base, Vec::new());
        let result = run(&session, &Query::Uses { name: "add".into() }).unwrap();
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains("in init  at t.c:3 into ops@8 (on_event)"),
            "got: {text:?}"
        );
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
            include_overlay: false,
            min_confidence: None,
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
        let text = render(&clean_callers_answer(), TextMode::Adaptive, Color::Never);
        assert!(
            !text.contains("note:"),
            "clean answer gained a footer: {text}"
        );
    }

    /// A callers-of-`b` answer with no footer notes: `session_from` leaves
    /// every function's `location` unset, which would itself trip the
    /// footer's "functions without a location" count, so this gives each
    /// function one instead.
    fn clean_callers_answer() -> QueryResult {
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
        run(&session, &Query::Callers { name: "b".into() }).unwrap()
    }

    #[test]
    fn a_cache_past_its_threshold_says_how_to_prune() {
        let mut result = clean_callers_answer();
        result.analysis.cache = Some(CacheReport {
            disk_bytes: 2 * MIB,
            warn_bytes: MIB,
            over_threshold: true,
            ..Default::default()
        });
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(
            text.contains(
                "facts cache is 2.0 MB, over query_cache_warn_mb (1 MB); prune with rllvm-query cache clear"
            ),
            "got: {text}"
        );
    }

    #[test]
    fn a_cache_under_its_threshold_prints_no_note() {
        let mut result = clean_callers_answer();
        result.analysis.cache = Some(CacheReport {
            disk_bytes: MIB / 2,
            warn_bytes: MIB,
            over_threshold: false,
            ..Default::default()
        });
        let text = render(&result, TextMode::Adaptive, Color::Never);
        assert!(!text.contains("facts cache"), "got: {text}");
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
            text.contains("5 of 5 modules were not analyzed: 3 failed, 2 missing"),
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
            debug_info: Some(true),
            ..report("m", ModuleAnalysis::Analyzed)
        }];
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        let text = render(&result, TextMode::Full, Color::Never);
        assert!(
            text.contains("whole_program_complete: false"),
            "got: {text}"
        );
        // Bare values, not `{:?}`'s `Some("linked")`/`Some(true)`: the
        // `Option` wrapper is how the fact is stored, not part of it.
        assert!(
            text.contains("ir_stage: linked debug_info: true"),
            "got: {text}"
        );
        assert!(text.contains("catalog: test"), "got: {text}");
    }

    #[test]
    fn full_mode_prints_unknown_for_absent_ir_stage_and_debug_info() {
        // `report` leaves both fields unset. `{:?}` used to print `None`,
        // which reads as a real value rather than as something the catalog
        // never recorded.
        let mut facts = facts(vec![], vec![]);
        facts.modules = vec![report("m", ModuleAnalysis::Analyzed)];
        let result = run(&Session::new(facts, vec![]), &Query::Externals).unwrap();
        let text = render(&result, TextMode::Full, Color::Never);
        assert!(
            text.contains("ir_stage: <unknown> debug_info: <unknown>"),
            "got: {text}"
        );
        assert!(!text.contains("None"), "got: {text}");
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
        assert!(text.contains("ambiguous"), "got: {text}");
        assert!(text.contains("2 candidate(s))"), "got: {text}");
    }

    #[test]
    fn a_binding_status_reads_the_same_word_in_text_as_in_json() {
        // Pins the `{:?}`-vs-serde divergence shut: the expected word comes
        // from `serde_json`'s own serialization of the value, not a
        // hardcoded lowercase literal, so a `BindingStatus` variant rename
        // fails this test instead of silently changing only one of the two
        // outputs.
        let session = session_with_ambiguous_bindings();
        let result = run(&session, &Query::Externals).unwrap();
        let binding = result
            .uncertainty
            .frontier
            .iter()
            .find(|binding| binding.symbol == "target")
            .expect("fixture changed: expected a frontier entry for `target`");
        let envelope_spelling = match serde_json::to_value(binding.status) {
            Ok(serde_json::Value::String(spelling)) => spelling,
            other => panic!("BindingStatus did not serialize to a JSON string: {other:?}"),
        };
        let text = render(&result, TextMode::Full, Color::Never);
        let frontier_line = text
            .lines()
            .find(|line| line.trim_start().starts_with("frontier: target"))
            .expect("fixture changed: expected a frontier line for `target`");
        assert!(
            frontier_line.contains(&envelope_spelling),
            "text `{frontier_line}` does not carry the envelope's `{envelope_spelling}`"
        );
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

    fn ffi_exports_text(functions: Vec<FunctionFact>) -> String {
        let session = Session::new(facts(functions, vec![]), vec![]);
        let result = run(&session, &Query::FfiExports).unwrap();
        render(&result, TextMode::Adaptive, Color::Never)
    }

    #[test]
    fn an_ffi_exports_answer_names_what_it_could_not_attribute() {
        let text = ffi_exports_text(vec![attributed("mystery", Linkage::External, None)]);
        assert!(
            text.contains(
                "1 unmangled definition(s) could not be attributed to a language and were not searched: they have no debug info"
            ),
            "got: {text}"
        );
        assert!(
            text.contains("no function attributed to Rust exports an unmangled symbol"),
            "got: {text}"
        );
    }

    #[test]
    fn an_ffi_exports_row_prints_like_a_definition() {
        let mut export = attributed("lib_add", Linkage::External, Some(Language::Rust));
        export.location = Some(source_location("lib.rs", 3));
        let text = ffi_exports_text(vec![export]);
        assert_eq!(
            text.lines().next().unwrap(),
            "lib.rs:3  lib_add",
            "got: {text}"
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
                include_overlay: false,
                min_confidence: None,
            },
        )
        .unwrap();
        assert!(result.results.is_empty(), "fixture changed");
        assert!(
            render(&result, TextMode::Adaptive, Color::Never)
                .contains("no functions were found in the selected scope for that direction")
        );
    }

    #[test]
    fn byte_counts_print_in_mb_below_a_gib_and_gb_above() {
        assert_eq!(human_bytes(0), "0.0 MB");
        assert_eq!(human_bytes(3_774_873), "3.6 MB");
        assert_eq!(human_bytes(GIB - 1), "1024.0 MB");
        assert_eq!(human_bytes(GIB), "1.0 GB");
        assert_eq!(human_bytes(1_363_148_800), "1.3 GB");
    }
}
