//! The `guards` query: the comparisons, clamps, masks and moduli that
//! constrain a value at a call-site argument, a call result, or a named
//! local.
//!
//! This reports evidence and never judges: an empty `guards` list means the
//! trace found nothing constraining the value, not that the value is
//! unbounded.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

use rllvm_core::{config::LlvmBindir, error::Error};

use crate::bind::collapse_odr_duplicates;
use crate::extract::read_module_ir;
use crate::ir::{
    BinaryOp, CallIr, CastOp, FunctionIr, GuardOpRaw, ModuleIr, OpcodeIr, Operand, ValueId,
    ValueNode, int_predicate,
};
use crate::overlay::{bare, function_label, site_label};
use crate::slice::{EmittedModule, check_opt_llvm, emit_module, promote, promote_anchor_module};
use crate::{
    CallSiteFact, CallSiteId, CallTarget, FunctionId, GuardKind, Linkage, Session, SourceLocation,
};

/// The value whose constraints are asked for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardAnchor {
    /// Operand `operand` of the call at `site`.
    Argument { site: CallSiteId, operand: usize },
    /// The result of the call at `site`.
    Return { site: CallSiteId },
    /// The source variable `name` in `function`.
    Local { function: String, name: String },
}

/// The value a flag combination names, before any call site is looked up.
enum AnchorShape<'a> {
    Argument { at: &'a str, operand: usize },
    Return { at: &'a str },
    Local { function: &'a str, name: &'a str },
}

/// The parts a surface's flags or arguments give for an anchor: `at` with
/// `operand` or `ret`, or `function` with `local`. Every surface fills this
/// in and hands it to [`anchor_from_parts`], so none validates on its own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnchorParts {
    pub at: Option<String>,
    pub operand: Option<usize>,
    pub ret: bool,
    pub function: Option<String>,
    pub local: Option<String>,
}

/// How a surface spells the parts of an anchor in its errors: the CLI's
/// flags, or the MCP tool's arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorSurface {
    Cli,
    Tool,
}

impl AnchorSurface {
    fn at(self) -> &'static str {
        self.pick("--at", "`at`")
    }
    fn operand(self) -> &'static str {
        self.pick("--operand", "`operand`")
    }
    fn ret(self) -> &'static str {
        self.pick("--return", "`return`")
    }
    fn function(self) -> &'static str {
        self.pick("--function", "`function`")
    }
    fn local(self) -> &'static str {
        self.pick("--local", "`local`")
    }
    fn pick(self, flag: &'static str, argument: &'static str) -> &'static str {
        match self {
            AnchorSurface::Cli => flag,
            AnchorSurface::Tool => argument,
        }
    }
}

impl AnchorParts {
    /// What these parts name, or why they do not name one. Needs no
    /// catalog, so a surface can reject a bad combination before loading one.
    /// `at` is the call site with `operand` or `ret`, and a use of the
    /// local with `function` and `local`.
    fn shape(&self, surface: AnchorSurface) -> Result<AnchorShape<'_>, String> {
        let (at, operand, ret) = (self.at.as_deref(), self.operand, self.ret);
        let (function, local) = (self.function.as_deref(), self.local.as_deref());
        let call = operand.is_some() || ret;
        let named = function.is_some() || local.is_some();
        let (n_at, n_operand, n_ret) = (surface.at(), surface.operand(), surface.ret());
        let (n_function, n_local) = (surface.function(), surface.local());
        match (call, named) {
            (false, false) => Err(match at {
                None => format!(
                    "name a value: {n_at} with {n_operand} or {n_ret}, or {n_function} with \
                     {n_local}"
                ),
                Some(_) => format!(
                    "{n_at} needs {n_operand} (an argument) or {n_ret} (the result), or \
                     {n_function} with {n_local} for a use of a local"
                ),
            }),
            (true, true) => Err(format!(
                "a call's value ({n_operand}, {n_ret}) and a local ({n_function}, {n_local}) \
                 name two values: give one"
            )),
            (true, false) => {
                let at = at.ok_or_else(|| {
                    format!("{n_operand} and {n_ret} need {n_at}, the call site they refer to")
                })?;
                match operand {
                    Some(_) if ret => {
                        Err(format!("{n_operand} and {n_ret} name two values: give one"))
                    }
                    Some(operand) => Ok(AnchorShape::Argument { at, operand }),
                    None => Ok(AnchorShape::Return { at }),
                }
            }
            (false, true) => match (function, local) {
                (Some(function), Some(name)) => {
                    if let Some(at) = at {
                        check_use_site(at, surface)?;
                    }
                    Ok(AnchorShape::Local { function, name })
                }
                (None, _) => Err(format!(
                    "{n_local} needs {n_function}, the function that holds it"
                )),
                (_, None) => Err(format!(
                    "{n_function} needs {n_local}, the variable to trace"
                )),
            },
        }
    }

    /// The use-site `at` names for a local: the `file:line` that picks one
    /// of a reused local's values. `None` for a call site, which `at`
    /// names instead.
    pub fn use_site(&self) -> Option<&str> {
        self.local.as_ref().and(self.at.as_deref())
    }
}

/// Rejects a local's use-site `at` that is not a `file:line`: a call site
/// id names one call, not a line that reads the local.
fn check_use_site(at: &str, surface: AnchorSurface) -> Result<(), String> {
    if at.trim_start().starts_with('{') {
        return Err(format!(
            "{} with {} is the `file:line` of a use of the local, not a call site id: an id \
             names a call, not a line that reads the local",
            surface.at(),
            surface.local()
        ));
    }
    crate::parse_location(at.trim()).map(|_| ()).map_err(bare)
}

/// Rejects a combination of anchor parts that names no value, without
/// reading a catalog. [`anchor_from_parts`] makes the same check.
pub fn check_anchor_parts(parts: &AnchorParts, surface: AnchorSurface) -> Result<(), String> {
    parts.shape(surface).map(|_| ())
}

/// The anchor a surface's parts name. `at` is a `file:line`, or a call site id as JSON
/// for a line that holds several calls. A line must hold exactly one call
/// site: with several, the error names each so the caller can pick one.
///
/// Surface-neutral on purpose: the CLI, the MCP tool and `annotate` all
/// resolve a call site the same way.
pub fn anchor_from_parts(
    session: &Session,
    parts: &AnchorParts,
    surface: AnchorSurface,
) -> Result<GuardAnchor, String> {
    Ok(match parts.shape(surface)? {
        AnchorShape::Argument { at, operand } => GuardAnchor::Argument {
            site: resolve_site(session, at)?,
            operand,
        },
        AnchorShape::Return { at } => GuardAnchor::Return {
            site: resolve_site(session, at)?,
        },
        AnchorShape::Local { function, name } => GuardAnchor::Local {
            function: function.to_string(),
            name: name.to_string(),
        },
    })
}

/// The one call site `at` names: a call site id spelled as JSON, which must
/// be in the catalog, or the single call site on a `file:line`.
fn resolve_site(session: &Session, at: &str) -> Result<CallSiteId, String> {
    let at = at.trim();
    if at.starts_with('{') {
        let id: CallSiteId =
            serde_json::from_str(at).map_err(|error| format!("invalid call site id: {error}"))?;
        return if session.call_sites().iter().any(|site| site.id == id) {
            Ok(id)
        } else {
            Err(format!("no such call site in the catalog: {at}"))
        };
    }
    let (file, line) = crate::parse_location(at).map_err(bare)?;
    let sites = session.call_sites_at(&file, line);
    match sites.as_slice() {
        [] => Err(format!(
            "no call site at {at}: the line may differ from the one the debug info \
             recorded, or the call may have been inlined away; check the recorded line, \
             or name the value with `--function F --local NAME`"
        )),
        [one] => Ok(one.id.clone()),
        several => Err(format!(
            "{at} holds {} call sites; name one by its call site id as JSON: {}",
            several.len(),
            several
                .iter()
                .map(|site| format!("{} => {}", candidate_label(site), id_json(&site.id)))
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

pub(crate) fn id_json(id: &CallSiteId) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

/// A call site by what it calls and where in its function: enough to pick it.
fn candidate_label(site: &crate::facts::CallSiteFact) -> String {
    let callee = match &site.target {
        CallTarget::Direct { callee } => callee.symbol.as_str(),
        _ => "an indirect or intrinsic call",
    };
    format!(
        "{callee} in {} (block {}, instruction {})",
        site.id.function.symbol, site.id.block_index, site.id.instruction_index
    )
}

/// How a guard relates to the anchored value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardRelation {
    /// The guard produces the value.
    Defines,
    /// The guard dominates the anchor.
    Dominates,
    /// The guard lies on some path to the anchor without dominating it.
    OnPath,
}

/// The relation `anchored = scale * guarded + offset` between the anchored
/// value and the value a guard bounds, over exact integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Affine {
    pub scale: i128,
    pub offset: i128,
}

impl Affine {
    /// The guard bounds the anchored value itself, or a widening of it.
    pub const IDENTITY: Affine = Affine {
        scale: 1,
        offset: 0,
    };
}

/// One instruction that constrains the anchored value, or a value the
/// anchored one is computed from: `depth` says which.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct GuardSite {
    pub op: GuardKind,
    /// Reads with the traced value on the left, `value ult bound`, however
    /// the instruction orders its operands. A saturating subtraction from
    /// the bound, `bound - value`, has a `.rev` suffix: `usub.sat.rev`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    /// The constant: unsigned for a mask and for an unsigned operation
    /// (`ult`, `umin`, `usub.sat`, `urem`, ...), at the width of the guarded
    /// integer; as the IR prints it, signed, otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
    /// `dominates` says the guard runs before the use on every path, not
    /// which way its branch goes to reach it.
    pub relation: GuardRelation,
    /// 0 when the guard constrains the anchored value itself, a widening of
    /// it (`zext`, `sext`), whose bound holds when the guard's signedness
    /// matches the cast's, or the value a store wrote that it was loaded
    /// back from. Otherwise how many steps up the chain the
    /// anchored value is computed from lies the value it constrains, a
    /// truncation counting as a step; such a guard is not by itself a bound
    /// on the anchored value: anchored at `n = (len & 0xff) + 1000`, the
    /// mask is at depth 1, and `affine` says how its bound carries to `n`.
    pub depth: u32,
    /// Whether the guard and its `affine` relation to the anchored value
    /// are both established. A site that is not is a lead, never a fact:
    /// `caveat` says what was not established.
    pub verified: bool,
    /// What keeps the site from being verified; present exactly when it is
    /// not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
    /// How the anchored value is computed from the value the guard bounds.
    /// Absent when it is not a constant affine function of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affine: Option<Affine>,
}

/// Which way a trace crossed a call boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossingDir {
    /// Into a callee, through a parameter.
    Into,
    /// Out of a callee, through its return value.
    From,
}

/// A call boundary the trace crossed.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct Crossing {
    pub site: CallSiteId,
    pub direction: CrossingDir,
    pub function: FunctionId,
    pub position: String,
}

/// The `results` of a `guards` query.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct GuardsResult {
    pub anchor: Option<GuardAnchor>,
    pub guards: Vec<GuardSite>,
    pub crossings: Vec<Crossing>,
    /// Why the trace stopped early, when it did: every reason, in the order
    /// met. The guards found before it stopped are still reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<String>,
    /// What LLVM reported while reading the anchor function's module, such
    /// as debug info it stripped: not a lost trace, but the module says less
    /// than it did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl GuardsResult {
    pub(crate) fn is_empty(&self) -> bool {
        self.guards.is_empty() && self.crossings.is_empty()
    }
}

/// Saturating operations whose operands do not commute, with how each reads
/// when the traced value is the right-hand operand: the bound minus it.
const REVERSED_SATURATES: [(&str, &str); 2] =
    [("usub.sat", "usub.sat.rev"), ("ssub.sat", "ssub.sat.rev")];

/// Casts after which the integer is no longer the value its guards bound:
/// one through a pointer, or the same bits read as another type.
const UNTRACED_CASTS: [(CastOp, &str); 4] = [
    (CastOp::PtrToInt, "ptrtoint"),
    (CastOp::PtrToAddr, "ptrtoaddr"),
    (CastOp::IntToPtr, "inttoptr"),
    (CastOp::BitCast, "bitcast"),
];

/// Casts that keep an integer's value, only widening it. A `trunc` is not
/// one: many values narrow to the same one, so a bound on the wide value is
/// none on the narrow.
const WIDENING_CASTS: [CastOp; 2] = [CastOp::ZExt, CastOp::SExt];

/// Intrinsics, by name prefix, whose result is computed from their integer
/// arguments alone: the trace reads through them to those arguments. Any
/// other intrinsic may read memory or a register, or compute from a pointer,
/// so the trace stops there and says so. Clamps and saturating operations
/// are guards, read before this list is.
const PURE_INTRINSICS: [&str; 23] = [
    "llvm.abs.",
    "llvm.smin.",
    "llvm.smax.",
    "llvm.umin.",
    "llvm.umax.",
    "llvm.sadd.sat.",
    "llvm.uadd.sat.",
    "llvm.ssub.sat.",
    "llvm.usub.sat.",
    "llvm.sshl.sat.",
    "llvm.ushl.sat.",
    "llvm.bswap.",
    "llvm.bitreverse.",
    "llvm.ctpop.",
    "llvm.ctlz.",
    "llvm.cttz.",
    "llvm.fshl.",
    "llvm.fshr.",
    "llvm.scmp.",
    "llvm.ucmp.",
    "llvm.expect.",
    "llvm.expect.with.probability.",
    "llvm.ssa.copy.",
];

/// How intrinsic names begin.
const INTRINSIC_PREFIX: &str = "llvm.";

/// The position a value crossing out of a callee holds.
const RETURN_POSITION: &str = "return";

/// What separates the reasons a trace was lost.
const LOST_SEPARATOR: &str = "; ";

/// What separates the reasons a guard site is not verified.
const CAVEAT_SEPARATOR: &str = "; ";

/// Why a guard site is not verified, in the order its caveat names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Unverified {
    /// A call runs between a store and the load that reads it back.
    CallBetween,
    /// Another store runs between them.
    StoreBetween,
    /// Some other operation that may write memory runs between them.
    OperationBetween,
    /// More than one store to the address runs before the load, none in
    /// its block.
    SeveralStores,
    /// The one store runs in another block than the load.
    OtherBlock,
    /// The anchored value is no constant affine function of the bounded one.
    NotAffine,
    /// The guard runs on the anchored value and bounds its own result, which
    /// the anchored value is not computed from.
    Downstream,
    /// Carried through the affine relation, the bound leaves the anchored
    /// value's range.
    Wraps,
    /// The bound gives no interval to carry: there is no constant, no width,
    /// no branch edge that decides which side of a comparison holds, or the
    /// constant is one the guard's integer cannot hold or passes no value.
    Unmappable,
}

impl Unverified {
    fn reason(self) -> &'static str {
        match self {
            Unverified::CallBetween => "a call between the store and the load may change the field",
            Unverified::StoreBetween => {
                "another store between the store and the load may change the field"
            }
            Unverified::OperationBetween => {
                "an operation between the store and the load may change the field"
            }
            Unverified::SeveralStores => "more than one store to this field reaches the load",
            Unverified::OtherBlock => "the store and the load are in different blocks",
            Unverified::NotAffine => "the relation to the anchored value is not affine",
            Unverified::Downstream => "the guard bounds a value computed from the anchored one",
            Unverified::Wraps => {
                "the bound does not carry to the anchored value without wraparound"
            }
            Unverified::Unmappable => "the bound could not be mapped to the anchored value",
        }
    }
}

/// Every cause in `causes`, in order.
fn reasons(causes: &BTreeSet<Unverified>) -> String {
    causes
        .iter()
        .map(|cause| cause.reason())
        .collect::<Vec<_>>()
        .join(CAVEAT_SEPARATOR)
}

/// Marks `site` verified exactly when nothing in `causes` keeps it from
/// being, and otherwise names every cause, in order, in its caveat.
fn mark(site: &mut GuardSite, causes: &BTreeSet<Unverified>) {
    site.verified = causes.is_empty();
    site.caveat = (!causes.is_empty()).then(|| reasons(causes));
}

/// The guards on the value `anchor` names, read from its function's own
/// module once `mem2reg` has promoted it. The trace walks the value's
/// definitions back within that function and stops at a call boundary,
/// which it reports as a crossing: into the function from each caller of
/// a parameter, bounded to the callers on a path from `from` when given,
/// and out of a callee whose result it is.
///
/// Evidence, never a verdict: no guard found is an empty list. Where the
/// trace cannot follow the value it says so in `lost` and still returns
/// what it found.
pub fn guards(
    session: &Session,
    anchor: &GuardAnchor,
    from: Option<&str>,
) -> Result<GuardsResult, Error> {
    guards_at_use(session, anchor, None, from)
}

/// [`guards`], with a local read at one use: `at`, a `file:line`, picks
/// which of the values a reused local is bound to is traced.
pub fn guards_at_use(
    session: &Session,
    anchor: &GuardAnchor,
    at: Option<&str>,
    from: Option<&str>,
) -> Result<GuardsResult, Error> {
    let function = anchor_function(session, anchor)?;
    let scope = from
        .map(|from| caller_scope(session, from, &function))
        .transpose()?;
    let promoted = promote_anchor_module(session, &function)?;
    let ir = read_module_ir(&promoted.bytes)?;
    let mut lost = Vec::new();
    let mut result = GuardsResult {
        anchor: Some(anchor.clone()),
        diagnostics: promoted
            .diagnostics
            .into_iter()
            .chain(ir.diagnostics.iter().cloned())
            .collect(),
        ..GuardsResult::default()
    };
    match resolve_anchor_at_use(&ir, session, anchor, at)? {
        Resolved::Ambiguous {
            function: index,
            values,
        } => {
            let body = &ir.functions[index];
            let uses = match anchor {
                GuardAnchor::Local { name, .. } => {
                    use_lines(body, &local_uses(body, &dominators(body), name))
                }
                GuardAnchor::Argument { .. } | GuardAnchor::Return { .. } => Vec::new(),
            };
            lost.push(format!(
                "{}, so none is traced{}",
                bound_to_several(anchor, &function, values.len()),
                match uses.as_slice() {
                    [] => String::new(),
                    lines => format!(": name one by a line that uses it, at {}", lines.join(", ")),
                }
            ));
        }
        Resolved::One {
            function: index,
            value,
            used_at,
        } => {
            let body = &ir.functions[index];
            let trace = Trace {
                session,
                body,
                function: &function,
                scope: scope.as_ref(),
                doms: dominators(body),
            };
            let at = match used_at {
                Some(at) => at,
                None => use_point(body, session, anchor, value)?,
            };
            let walk = trace.walk(value);
            result.guards = trace.guard_sites(&walk, value, at);
            result.crossings = walk.crossings;
            lost.extend(walk.lost);
        }
    }
    let mut seen = HashSet::new();
    lost.retain(|reason| seen.insert(reason.clone()));
    result.lost = (!lost.is_empty()).then(|| lost.join(LOST_SEPARATOR));
    Ok(result)
}

/// Confirms `anchor` names one value to trace, read as [`guards`] reads it:
/// in its function's `mem2reg`-promoted module. An operand must be an
/// in-range value, a call result not void, and a local bound to exactly one
/// value; anything else is an error saying why. A session that cannot
/// promote the module -- no catalog file, no LLVM bindir -- says which is
/// missing.
pub(crate) fn ground_anchor(session: &Session, anchor: &GuardAnchor) -> Result<(), Error> {
    let function = anchor_function(session, anchor)?;
    let promoted = promote_anchor_module(session, &function)?;
    let ir = read_module_ir(&promoted.bytes)?;
    match resolve_anchor(&ir, session, anchor)? {
        Resolved::One { .. } => Ok(()),
        Resolved::Ambiguous { values, .. } => Err(Error::InvalidArguments(format!(
            "{}, so there is no one value for a verdict to rest on",
            bound_to_several(anchor, &function, values.len())
        ))),
    }
}

/// What [`Resolved::Ambiguous`] means for `anchor`, a local of `function`
/// bound to `values` SSA values among others.
fn bound_to_several(anchor: &GuardAnchor, function: &FunctionId, values: usize) -> String {
    let name = match anchor {
        GuardAnchor::Local { name, .. } => name.as_str(),
        GuardAnchor::Argument { .. } | GuardAnchor::Return { .. } => "?",
    };
    format!(
        "local `{name}` in {} is bound to more than one value over its lifetime ({values} of \
         them SSA values)",
        function_label(function)
    )
}

/// Writes the slice a scoped re-read of `anchor` would read as one
/// `mem2reg`-promoted module at `out`: the `from` to anchor-function slice
/// when `from` is set, else the anchor function and its callers. The analysis
/// itself never links; this is the one place `guards` does.
pub fn emit_guards_module(
    session: &Session,
    catalog: &Path,
    anchor: &GuardAnchor,
    from: Option<&str>,
    llvm_link: &Path,
    llvm_bindir: &LlvmBindir,
    out: &Path,
) -> Result<EmittedModule, Error> {
    let function = anchor_function(session, anchor)?;
    let functions: Vec<FunctionId> = match from {
        Some(from) => {
            let mut scope: Vec<FunctionId> = caller_scope(session, from, &function)?
                .into_iter()
                .collect();
            scope.sort();
            scope
        }
        None => {
            let mut set = BTreeSet::from([function.clone()]);
            set.extend(
                calls_to(session, &function)
                    .into_iter()
                    .map(|site| site.id.function),
            );
            set.into_iter().collect()
        }
    };
    // Before llvm-link: `opt` must read every module the slice is cut from.
    let modules: BTreeSet<&str> = functions.iter().map(|id| id.module_id.as_str()).collect();
    for module in modules {
        check_opt_llvm(llvm_bindir, session, module)?;
    }
    // Linked and promoted in scratch, then renamed into place: a failure
    // on the way leaves nothing at `out`.
    let scratch = tempfile::TempDir::new()?;
    let linked = scratch.path().join("linked.bc");
    let emitted = emit_module(
        session,
        catalog,
        &functions,
        llvm_link,
        llvm_bindir,
        &linked,
    )?;
    let promoted = promote(&std::fs::read(&linked)?, llvm_bindir, scratch.path())?;
    let directory = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    // Created as `llvm-link` creates a slice's module: readable as the
    // umask allows, not owner-only as a temporary file would be.
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut staged = builder.tempfile_in(directory)?;
    std::io::Write::write_all(&mut staged, &promoted.bytes)?;
    staged.persist(out).map_err(|error| error.error)?;
    Ok(EmittedModule {
        path: out.to_path_buf(),
        ..emitted
    })
}

/// The functions on a path from `from` to `function`: the callers a
/// parameter may cross from when the trace is bounded to that slice. `from`
/// must name exactly one defined function. An error when `function` is on
/// no such path.
pub(crate) fn caller_scope(
    session: &Session,
    from: &str,
    function: &FunctionId,
) -> Result<HashSet<FunctionId>, Error> {
    let start = one_definition(session, from, "`from`")?;
    let scope: HashSet<FunctionId> = session
        .slice_with(&start.symbol, &function.symbol, None)
        .functions
        .into_iter()
        .collect();
    if !scope.contains(function) {
        return Err(Error::InvalidArguments(format!(
            "{} is not on a path from {from:?} over the call graph, so there is no slice to \
             bound the trace to",
            function_label(function)
        )));
    }
    Ok(scope)
}

/// The instruction the anchored `value` is used at: the call, for a call
/// site's argument or result; for a local, the first binding to `value`.
fn use_point(
    body: &FunctionIr,
    session: &Session,
    anchor: &GuardAnchor,
    value: ValueId,
) -> Result<ValueId, Error> {
    Ok(match anchor {
        GuardAnchor::Argument { site, .. } | GuardAnchor::Return { site } => {
            promoted_call(body, session, site)?.0
        }
        GuardAnchor::Local { name, .. } => body
            .dbg_values
            .iter()
            .find(|binding| binding.name == *name && binding.value == Operand::Value(value))
            .map_or(value, |binding| binding.at),
    })
}

/// The call sites that run `function` itself: those whose target resolves,
/// through binding and aliases, to this definition. `Session::callers` goes
/// by name, so it also holds the callers of a same-named `static` elsewhere.
fn calls_to(session: &Session, function: &FunctionId) -> Vec<CallSiteFact> {
    let calls_traced = |callee: &FunctionId| definition_of(session, callee) == *function;
    session
        .callers(&function.symbol)
        .into_iter()
        .filter(|site| match &site.target {
            CallTarget::Direct { callee } => calls_traced(callee),
            CallTarget::Indirect {
                llvm_target_bound: Some(bound),
                ..
            } => bound.iter().any(calls_traced),
            _ => false,
        })
        .collect()
}

/// The definition `callee` runs: the one its declaration binds to, through
/// an alias to the function it stands for. The callee as referenced when
/// nothing binds it.
fn definition_of(session: &Session, callee: &FunctionId) -> FunctionId {
    let Some(definition) = session.bound_definition(callee) else {
        return callee.clone();
    };
    session
        .function(&definition)
        .and_then(|function| function.alias_of.clone())
        .unwrap_or(definition)
}

/// One function's trace of a value.
struct Trace<'a> {
    session: &'a Session,
    body: &'a FunctionIr,
    /// The catalog's identity of `body`.
    function: &'a FunctionId,
    /// When the trace is bounded by `from`: the functions whose calls a
    /// parameter may cross from.
    scope: Option<&'a HashSet<FunctionId>>,
    doms: Doms,
}

/// What a backward walk over a value's definitions found.
#[derive(Default)]
struct Walk {
    /// Every value the anchored one is computed from, itself included.
    chain: BTreeSet<ValueId>,
    /// The guards among them, by the instruction that makes each.
    defining: BTreeMap<ValueId, GuardSite>,
    /// Comparisons a clamp in the chain is made of, which it reports.
    folded: HashSet<ValueId>,
    /// Each step the walk took back: a value, and an operand it is
    /// computed from.
    steps: Vec<(ValueId, ValueId)>,
    crossings: Vec<Crossing>,
    lost: Vec<String>,
    /// Each value a load may not read back from its store, by the value
    /// stored, with why: every guard on it, or on what it is computed from,
    /// is a candidate.
    unproven: BTreeMap<ValueId, BTreeSet<Unverified>>,
}

impl Walk {
    /// The operands the walk followed back from each value, in `steps`
    /// order.
    fn followed(&self) -> HashMap<ValueId, Vec<ValueId>> {
        let mut operands: HashMap<ValueId, Vec<ValueId>> = HashMap::new();
        for (user, operand) in &self.steps {
            operands.entry(*user).or_default().push(*operand);
        }
        operands
    }
}

/// How a guard reads the integers it bounds, which decides the widenings
/// its bound survives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signedness {
    Unsigned,
    Signed,
    /// `eq` and `ne`, which read the bits alone.
    Equality,
}

/// How `op` with `predicate` reads its integers: a mask and an operation
/// whose name starts with `u` unsigned, one starting with `s` signed. `None`
/// when the predicate says neither.
fn signedness(op: GuardKind, predicate: Option<&str>) -> Option<Signedness> {
    if op == GuardKind::Mask {
        return Some(Signedness::Unsigned);
    }
    match predicate? {
        "eq" | "ne" => Some(Signedness::Equality),
        p if p.starts_with('u') => Some(Signedness::Unsigned),
        p if p.starts_with('s') => Some(Signedness::Signed),
        _ => None,
    }
}

/// The widenings between a value and the anchored one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Widenings {
    zext: bool,
    sext: bool,
}

impl Widenings {
    const NONE: Widenings = Widenings {
        zext: false,
        sext: false,
    };

    fn with(self, op: CastOp) -> Self {
        Widenings {
            zext: self.zext || op == CastOp::ZExt,
            sext: self.sext || op == CastOp::SExt,
        }
    }

    /// Whether `site`'s bound holds across all of them: a `zext` keeps an
    /// unsigned value, a `sext` a signed one. An equality's bound is printed
    /// signed, which a `sext` keeps, and a `zext` only when it is not
    /// negative.
    fn keep(self, site: &GuardSite) -> bool {
        if self == Widenings::NONE {
            return true;
        }
        match signedness(site.op, site.predicate.as_deref()) {
            Some(Signedness::Unsigned) => !self.sext,
            Some(Signedness::Signed) => !self.zext,
            Some(Signedness::Equality) => {
                !self.zext
                    || site
                        .bound
                        .as_deref()
                        .and_then(|bound| bound.parse::<i128>().ok())
                        .is_some_and(|bound| bound >= 0)
            }
            None => false,
        }
    }
}

/// How the anchored value is computed from one value of its chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relation {
    /// The anchored value is not computed from it.
    Unrelated,
    /// Through constant `add`, `sub`, `mul` and `shl` alone, at one width:
    /// `anchored = scale * value + offset`.
    Arithmetic(Affine),
    /// Through widenings alone, or none: the anchored value is the value.
    Widened(Widenings),
    /// Through some step that is no constant affine function.
    NotAffine,
}

impl Relation {
    /// The affine relation `site`'s guard carries to the anchored value: a
    /// widening is the identity only when it keeps the guard's bound.
    fn affine(self, site: &GuardSite) -> Option<Affine> {
        match self {
            Relation::Arithmetic(affine) => Some(affine),
            Relation::Widened(widenings) if widenings.keep(site) => Some(Affine::IDENTITY),
            Relation::Widened(_) | Relation::Unrelated | Relation::NotAffine => None,
        }
    }
}

/// `op` with one constant operand as an affine function of the other, and
/// that operand: `None` for any other operation, or a shift by the width or
/// more, whose result is poison.
fn linear(op: BinaryOp, operands: &[Operand]) -> Option<(ValueId, Affine)> {
    let (operand, constant, constant_first) = match operands {
        [Operand::Value(operand), Operand::Const(constant)] => (*operand, constant, false),
        [Operand::Const(constant), Operand::Value(operand)] => (*operand, constant, true),
        _ => return None,
    };
    let c: i128 = constant.value.parse().ok()?;
    let affine = match (op, constant_first) {
        (BinaryOp::Add, _) => Affine {
            scale: 1,
            offset: c,
        },
        (BinaryOp::Sub, false) => Affine {
            scale: 1,
            offset: c.checked_neg()?,
        },
        (BinaryOp::Sub, true) => Affine {
            scale: -1,
            offset: c,
        },
        (BinaryOp::Mul, _) => Affine {
            scale: c,
            offset: 0,
        },
        (BinaryOp::Shl, false) => {
            let width = constant.bits?;
            let shift = u32::try_from(c).ok().filter(|shift| *shift < width)?;
            Affine {
                scale: 1_i128.checked_shl(shift).filter(|scale| *scale > 0)?,
                offset: 0,
            }
        }
        _ => return None,
    };
    Some((operand, affine))
}

/// Why `site`'s bound does not carry onto the anchored value through its
/// affine relation, or `None` when it does: every value the guard admits,
/// from one end of its interval to the other, must map into the anchored
/// value's range. For a comparison, `edge` is which way its branch goes to
/// reach the anchor, if one edge decides it.
///
/// The guard's integer of `bits` is the anchored value's width: an affine
/// relation other than the identity is arithmetic alone, which keeps one
/// width, and the identity maps the interval to itself. The guard's
/// signedness is the anchored value's reading, and the one its bound is
/// read in. Without an interval to carry -- no constant bound the guard's
/// integer holds, no width, no deciding edge -- nothing is known to carry.
fn uncarried(site: &GuardSite, bits: Option<u32>, edge: Option<bool>) -> Option<Unverified> {
    let affine = site.affine.filter(|affine| *affine != Affine::IDENTITY)?;
    let carries = || {
        let signed = match signedness(site.op, site.predicate.as_deref())? {
            Signedness::Unsigned => false,
            // An equality's bound is printed signed.
            Signedness::Signed | Signedness::Equality => true,
        };
        let bits = bits?;
        let range = range(bits, signed)?;
        let bound = read_bound(site.bound.as_deref()?, bits, signed)?;
        let (low, high) = interval(site, bound, range, edge)?;
        let map = |end: i128| affine.scale.checked_mul(end)?.checked_add(affine.offset);
        let (a, b) = (map(low)?, map(high)?);
        // A negative scale swaps the ends.
        Some(range.0 <= a.min(b) && a.max(b) <= range.1)
    };
    match carries() {
        Some(true) => None,
        Some(false) => Some(Unverified::Wraps),
        None => Some(Unverified::Unmappable),
    }
}

/// The values an integer of `bits` holds, read `signed` or not, when `i128`
/// holds them all.
fn range(bits: u32, signed: bool) -> Option<(i128, i128)> {
    if signed {
        let max = i128::MAX.checked_shr(128_u32.checked_sub(bits)?)?;
        (bits > 0).then_some((-max - 1, max))
    } else {
        let max = i128::MAX.checked_shr(127_u32.checked_sub(bits)?)?;
        (bits > 0).then_some((0, max))
    }
}

/// `bound`, a constant as the IR prints it or as [`unsigned_bound`] reads
/// it, as an integer of `bits` read `signed` or not. The IR prints every
/// integer signed, so unsigned, `-k` is `2^bits - k`. `None` when it is no
/// number, or one such an integer cannot hold.
fn read_bound(bound: &str, bits: u32, signed: bool) -> Option<i128> {
    let printed: i128 = bound.parse().ok()?;
    let (min, max) = range(bits, signed)?;
    let value = if signed || printed >= 0 {
        printed
    } else {
        let (lowest, _) = range(bits, true)?;
        if printed < lowest {
            return None;
        }
        // `printed + 2^bits`, without forming 2^bits, which an i128 does
        // not hold at 127 bits.
        printed.checked_add(max)?.checked_add(1)?
    };
    (min <= value && value <= max).then_some(value)
}

/// The values `site`'s guard with constant `bound` admits for the value it
/// bounds, read in `range`, the bounded integer's: an end the guard does not
/// fix is the type's extreme. A comparison admits what holds on the edge
/// `edge` names -- its predicate on the true one, the inverse on the false
/// -- and nothing without one. `None` when the guard passes no value, as
/// `x ult 0` does.
fn interval(
    site: &GuardSite,
    bound: i128,
    (min, max): (i128, i128),
    edge: Option<bool>,
) -> Option<(i128, i128)> {
    let predicate = match site.op {
        GuardKind::Compare => {
            let predicate = site.predicate.as_deref()?;
            if edge? {
                predicate
            } else {
                int_predicate(predicate)?.inverse
            }
        }
        _ => site.predicate.as_deref().unwrap_or_default(),
    };
    let (low, high) = match (site.op, predicate) {
        (GuardKind::Compare, "ult" | "slt") => (min, bound.checked_sub(1)?),
        (GuardKind::Compare, "ule" | "sle") | (GuardKind::Clamp, "umin" | "smin") => (min, bound),
        (GuardKind::Compare, "ugt" | "sgt") => (bound.checked_add(1)?, max),
        (GuardKind::Compare, "uge" | "sge") | (GuardKind::Clamp, "umax" | "smax") => (bound, max),
        (GuardKind::Compare, "eq") => (bound, bound),
        (GuardKind::Mask, _) => (0, bound),
        (GuardKind::Modulo, "urem") => (0, bound.checked_sub(1)?),
        (GuardKind::Modulo, "srem") => {
            let magnitude = bound.checked_abs()?.checked_sub(1)?;
            (-magnitude, magnitude)
        }
        // `ne` and the saturating operations fix neither end here.
        _ => (min, max),
    };
    (low <= high).then_some((low, high))
}

/// `outer` applied after `inner`, exactly: `None` when a term overflows
/// `i128`.
fn compose(outer: Affine, inner: Affine) -> Option<Affine> {
    Some(Affine {
        scale: outer.scale.checked_mul(inner.scale)?,
        offset: outer
            .scale
            .checked_mul(inner.offset)?
            .checked_add(outer.offset)?,
    })
}

/// Why each value of `walk`'s chain may not be what the anchored value is
/// computed from: the causes of every unproven load link on a path to it,
/// read along the steps `followed` holds.
fn unproven_upstream(
    walk: &Walk,
    followed: &HashMap<ValueId, Vec<ValueId>>,
) -> HashMap<ValueId, BTreeSet<Unverified>> {
    let mut unproven: HashMap<ValueId, BTreeSet<Unverified>> = HashMap::new();
    for (stored, causes) in &walk.unproven {
        let mut pending = vec![*stored];
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            unproven.entry(id).or_default().extend(causes);
            pending.extend(followed.get(&id).into_iter().flatten());
        }
    }
    unproven
}

/// The stores a load reads back, which [`Trace::link`] finds.
struct Link {
    /// The values they write.
    stored: Vec<Operand>,
    /// Why the load may not read what they wrote; empty when it must.
    causes: BTreeSet<Unverified>,
}

/// How a value of the chain is computed from the operands the walk followed
/// from it.
enum Step {
    /// The value is one of `arms`, whichever ran: a `phi`'s incoming values,
    /// a `select`'s arms.
    Choice(Vec<Operand>),
    /// The value is `affine` of `operand`, at the same width.
    Linear { operand: ValueId, affine: Affine },
    /// The value widens `operand`.
    Widen { operand: ValueId, op: CastOp },
    /// Anything else: the value is computed from its followed operands, but
    /// no constant affine function of any.
    Opaque,
}

impl Trace<'_> {
    fn node(&self, id: ValueId) -> &ValueNode {
        &self.body.values[id as usize]
    }

    /// Walks back from `value` through every operand it is computed from,
    /// recording the guards that make it, the call boundaries it crosses,
    /// and where it cannot be followed.
    fn walk(&self, value: ValueId) -> Walk {
        let mut walk = Walk::default();
        let mut pending = vec![value];
        while let Some(id) = pending.pop() {
            if !walk.chain.insert(id) {
                continue;
            }
            let node = self.node(id);
            let mut follow = |operands: &[Operand]| {
                for operand in operands {
                    if let Operand::Value(value) = operand {
                        pending.push(*value);
                        walk.steps.push((id, *value));
                    }
                }
            };
            match node.opcode {
                OpcodeIr::Param => {
                    let index = self.body.params.iter().position(|&param| param == id);
                    walk.crossings
                        .extend(self.crossings_into(index.unwrap_or_default()));
                }
                OpcodeIr::Call => {
                    let Some(call) = &node.call else { continue };
                    if call.inline_asm {
                        walk.lost.push(format!(
                            "inline assembly produces the value{}, which the IR does not describe",
                            place(node)
                        ));
                    } else if let Some(raw) = &node.guard {
                        walk.defining.insert(id, defining(raw, node, &call.args));
                        follow(&call.args);
                    } else {
                        match &call.callee {
                            Some(callee) if callee.symbol.starts_with(INTRINSIC_PREFIX) => {
                                if PURE_INTRINSICS
                                    .iter()
                                    .any(|prefix| callee.symbol.starts_with(prefix))
                                {
                                    follow(&call.args);
                                } else {
                                    walk.lost.push(format!(
                                        "the intrinsic `{}` produces the value{}, and the trace \
                                         does not read through it: it may read memory or a \
                                         register",
                                        callee.symbol,
                                        place(node)
                                    ));
                                }
                            }
                            Some(_) => match self.crossing_from(id) {
                                Ok(crossing) => walk.crossings.push(crossing),
                                Err(reason) => walk.lost.push(reason),
                            },
                            None => walk.lost.push(format!(
                                "an indirect call returns the value{}, and its callee is not known",
                                place(node)
                            )),
                        }
                    }
                }
                OpcodeIr::CallBr => walk.lost.push(format!(
                    "asm goto produces the value{}, which the IR does not describe",
                    place(node)
                )),
                OpcodeIr::Cast { op } => {
                    match UNTRACED_CASTS.iter().find(|(untraced, _)| *untraced == op) {
                        Some((_, name)) => walk.lost.push(format!(
                            "the value goes through `{name}`{}, after which its guards no longer \
                             bound it",
                            place(node)
                        )),
                        None => follow(&node.operands),
                    }
                }
                OpcodeIr::Select => {
                    if let Some(raw) = &node.guard {
                        walk.defining
                            .insert(id, defining(raw, node, &node.operands));
                        if let Some(Operand::Value(compare)) = node.operands.first() {
                            walk.folded.insert(*compare);
                        }
                    }
                    // The condition picks an arm; the arms are the value.
                    follow(node.operands.get(1..).unwrap_or_default());
                }
                OpcodeIr::Phi => {
                    if let Some((site, compare)) = self.phi_clamp(node) {
                        walk.defining.insert(id, site);
                        walk.folded.insert(compare);
                    }
                    follow(&node.operands);
                }
                OpcodeIr::BinOp { .. } | OpcodeIr::ICmp => {
                    if let Some(raw) = &node.guard {
                        walk.defining
                            .insert(id, defining(raw, node, &node.operands));
                    }
                    follow(&node.operands);
                }
                OpcodeIr::Load => match self.link(id) {
                    Some(link) => {
                        follow(&link.stored);
                        if !link.causes.is_empty() {
                            // The guards found through it are leads, and
                            // what else the field may hold is not traced.
                            walk.lost.push(format!(
                                "the value is loaded from memory{}, and the field may hold \
                                 something other than what the store wrote ({})",
                                place(node),
                                reasons(&link.causes)
                            ));
                            for stored in &link.stored {
                                if let Operand::Value(stored) = stored {
                                    walk.unproven
                                        .entry(*stored)
                                        .or_default()
                                        .extend(&link.causes);
                                }
                            }
                        }
                    }
                    None => walk.lost.push(format!(
                        "the value is loaded from memory{}, and what was stored there is not \
                         traced",
                        place(node)
                    )),
                },
                OpcodeIr::Gep | OpcodeIr::Other => walk.lost.push(format!(
                    "the value comes from an instruction the trace does not read through{}",
                    place(node)
                )),
                // None of these produces a value another instruction uses.
                OpcodeIr::Store | OpcodeIr::Ret | OpcodeIr::Br => {}
            }
        }
        walk
    }

    /// The guards `walk` found; then each guard op that consumes the
    /// anchored `value` downstream, which lies on its path; then each
    /// comparison on a value of the chain, or on `value` downstream, that
    /// does not make it: dominating when it runs before `at`, the
    /// instruction the value is used at, and on its path otherwise. In
    /// instruction order.
    ///
    /// A downstream clamp bounds its own result, so the comparison it is
    /// made of, when it is made of one, is listed too: it compares the
    /// anchored value or a widening of it. Its bound holds only on the side
    /// of its branch that reaches `at`, so without such an edge it is a
    /// lead.
    fn guard_sites(&self, walk: &Walk, value: ValueId, at: ValueId) -> Vec<GuardSite> {
        let depths = self.depths(walk, value);
        let depth_of = |id: &ValueId| depths.get(id).copied().unwrap_or_default();
        let mut sites = walk.defining.clone();
        // The value each site bounds, which its affine relation is read from.
        let mut bounds: HashMap<ValueId, ValueId> = HashMap::new();
        for (id, site) in &mut sites {
            site.depth = depth_of(id);
            if let Some(bounded) = self.bounded(*id) {
                bounds.insert(*id, bounded);
            }
        }
        let downstream = self.downstream(value);
        let mut downstream_guards: HashSet<ValueId> = HashSet::new();
        // The comparisons those guards are made of.
        let mut downstream_compares: HashSet<ValueId> = HashSet::new();
        let traced = |operand: &Operand| {
            matches!(operand, Operand::Value(value)
                if walk.chain.contains(value) || downstream.contains(value))
        };
        for (id, node) in self.body.values.iter().enumerate() {
            let id = id as ValueId;
            if walk.chain.contains(&id) || node.opcode == OpcodeIr::ICmp {
                continue;
            }
            let operands = node.call.as_ref().map_or(&node.operands, |call| &call.args);
            let Some(side) = operands.iter().position(
                |operand| matches!(operand, Operand::Value(used) if downstream.contains(used)),
            ) else {
                continue;
            };
            // Each guard here bounds its own result, which is computed from
            // the anchored value rather than the other way round.
            if node.opcode == OpcodeIr::Phi {
                if let Some((mut site, compare)) = self.phi_clamp(node) {
                    site.relation = GuardRelation::OnPath;
                    sites.insert(id, site);
                    downstream_guards.insert(id);
                    downstream_compares.insert(compare);
                }
            } else if let Some(raw) = &node.guard {
                sites.insert(
                    id,
                    surfaced(raw, side == 1, node.location.clone(), GuardRelation::OnPath),
                );
                downstream_guards.insert(id);
                if let (OpcodeIr::Select, Some(Operand::Value(compare))) =
                    (node.opcode, node.operands.first())
                {
                    downstream_compares.insert(*compare);
                }
            }
        }
        for (id, node) in self.body.values.iter().enumerate() {
            let id = id as ValueId;
            if node.opcode != OpcodeIr::ICmp || sites.contains_key(&id) || walk.folded.contains(&id)
            {
                continue;
            }
            let Some(raw) = &node.guard else { continue };
            let Some(side) = node.operands.iter().position(traced) else {
                continue;
            };
            let relation = if self.runs_before(id, at) {
                GuardRelation::Dominates
            } else {
                GuardRelation::OnPath
            };
            let mut site = surfaced(raw, side == 1, node.location.clone(), relation);
            if let Operand::Value(compared) = &node.operands[side] {
                if !downstream.contains(compared) {
                    site.depth = depth_of(compared);
                }
                bounds.insert(id, *compared);
            }
            sites.insert(id, site);
        }
        let followed = walk.followed();
        let unproven = unproven_upstream(walk, &followed);
        let mut relations: HashMap<ValueId, Relation> = HashMap::new();
        for (id, site) in &mut sites {
            if downstream_guards.contains(id) {
                site.affine = None;
                mark(site, &BTreeSet::from([Unverified::Downstream]));
                continue;
            }
            let relation = match bounds.get(id) {
                Some(&bounded) if downstream.contains(&bounded) => {
                    self.widened_from(value, bounded)
                }
                Some(&bounded) => *relations
                    .entry(bounded)
                    .or_insert_with(|| self.relation(&followed, value, bounded)),
                None => Relation::NotAffine,
            };
            site.affine = relation.affine(site);
            let mut causes = bounds
                .get(id)
                .and_then(|bounded| unproven.get(bounded))
                .cloned()
                .unwrap_or_default();
            if site.affine.is_none() {
                causes.insert(Unverified::NotAffine);
            } else {
                let edge = self.edge_to(*id, at);
                if edge.is_none() && downstream_compares.contains(id) {
                    causes.insert(Unverified::Unmappable);
                }
                causes.extend(uncarried(site, self.guard_width(*id), edge));
            }
            mark(site, &causes);
        }
        sites.into_values().collect()
    }

    /// Which edge of the branch on comparison `compare` reaches `at`: `true`
    /// for its true successor, `false` for its false one, when that
    /// successor dominates `at`'s block and is entered by that edge alone.
    /// `None` when no branch on it has such an edge.
    fn edge_to(&self, compare: ValueId, at: ValueId) -> Option<bool> {
        let target = self.node(at).block;
        let entries = |block: usize| {
            self.body
                .blocks
                .iter()
                .flat_map(|ir| &ir.successors)
                .filter(|&&successor| successor == block)
                .count()
        };
        (0..self.body.blocks.len()).find_map(|block| {
            let (branched, on_true, on_false) = self.branch_on(block)?;
            if branched != compare {
                return None;
            }
            [(on_true, true), (on_false, false)]
                .into_iter()
                .find_map(|(successor, taken)| {
                    (entries(successor) == 1 && self.doms.dominates(successor, target))
                        .then_some(taken)
                })
        })
    }

    /// The width in bits of the integer the guard at `id` operates on: its
    /// operation's, or, for a `phi` read as a clamp, its constant's.
    fn guard_width(&self, id: ValueId) -> Option<u32> {
        let node = self.node(id);
        node.guard.as_ref().map_or_else(
            || {
                node.operands.iter().find_map(|operand| match operand {
                    Operand::Const(constant) => constant.bits,
                    _ => None,
                })
            },
            |guard| guard.bits,
        )
    }

    /// The value the guard that makes `id` bounds: its result, or, for a
    /// comparison, the operand it reads as the traced value.
    fn bounded(&self, id: ValueId) -> Option<ValueId> {
        let node = self.node(id);
        if node.opcode != OpcodeIr::ICmp {
            return Some(id);
        }
        node.operands.iter().find_map(|operand| match operand {
            Operand::Value(value) => Some(*value),
            _ => None,
        })
    }

    /// How `id` is computed from the operands the walk followed from it.
    fn step(&self, id: ValueId) -> Step {
        let node = self.node(id);
        match node.opcode {
            OpcodeIr::Phi => Step::Choice(node.operands.clone()),
            // The load reads back what the store wrote.
            OpcodeIr::Load => self
                .link(id)
                .map_or(Step::Opaque, |link| Step::Choice(link.stored)),
            OpcodeIr::Select => Step::Choice(node.operands.get(1..).unwrap_or_default().to_vec()),
            OpcodeIr::Cast { op } if WIDENING_CASTS.contains(&op) => match node.operands.first() {
                Some(Operand::Value(operand)) => Step::Widen {
                    operand: *operand,
                    op,
                },
                _ => Step::Opaque,
            },
            OpcodeIr::BinOp { op } => {
                linear(op, &node.operands).map_or(Step::Opaque, |(operand, affine)| Step::Linear {
                    operand,
                    affine,
                })
            }
            _ => Step::Opaque,
        }
    }

    /// How the anchored `value` is computed from `guarded`, a value of its
    /// chain, over the steps `followed` holds: each value's relation to
    /// `guarded` read off its operands', from `guarded` up.
    fn relation(
        &self,
        followed: &HashMap<ValueId, Vec<ValueId>>,
        value: ValueId,
        guarded: ValueId,
    ) -> Relation {
        // `None` while a value's operands are pending: a cycle back to it
        // reads as no affine function, as a loop-carried value is none.
        let mut known: HashMap<ValueId, Option<Relation>> = HashMap::new();
        let mut pending: Vec<(ValueId, Option<Step>)> = vec![(value, None)];
        while let Some((id, step)) = pending.pop() {
            let Some(step) = step else {
                if known.contains_key(&id) {
                    continue;
                }
                known.insert(id, None);
                if id == guarded {
                    known.insert(id, Some(Relation::Widened(Widenings::NONE)));
                    continue;
                }
                let step = self.step(id);
                let inputs: Vec<ValueId> = match &step {
                    Step::Choice(arms) => arms
                        .iter()
                        .filter_map(|arm| match arm {
                            Operand::Value(arm) => Some(*arm),
                            _ => None,
                        })
                        .collect(),
                    Step::Linear { operand, .. } | Step::Widen { operand, .. } => vec![*operand],
                    Step::Opaque => followed.get(&id).cloned().unwrap_or_default(),
                };
                pending.push((id, Some(step)));
                pending.extend(
                    inputs
                        .into_iter()
                        .filter(|input| !known.contains_key(input))
                        .map(|input| (input, None)),
                );
                continue;
            };
            let of = |input: ValueId| {
                known
                    .get(&input)
                    .copied()
                    .flatten()
                    .unwrap_or(Relation::NotAffine)
            };
            let relation = match step {
                // One relation only when every arm has it: an arm that
                // differs, or is not computed from `guarded`, decides the
                // value on some path.
                Step::Choice(arms) => {
                    let relations: Vec<Relation> = arms
                        .iter()
                        .map(|arm| match arm {
                            Operand::Value(arm) => of(*arm),
                            _ => Relation::Unrelated,
                        })
                        .collect();
                    match relations.split_first() {
                        Some((first, rest)) if rest.iter().all(|arm| arm == first) => *first,
                        Some(_) => Relation::NotAffine,
                        None => Relation::Unrelated,
                    }
                }
                Step::Linear { operand, affine } => match of(operand) {
                    Relation::Widened(widenings) if widenings == Widenings::NONE => {
                        Relation::Arithmetic(affine)
                    }
                    Relation::Arithmetic(below) => {
                        compose(affine, below).map_or(Relation::NotAffine, Relation::Arithmetic)
                    }
                    Relation::Unrelated => Relation::Unrelated,
                    Relation::Widened(_) | Relation::NotAffine => Relation::NotAffine,
                },
                Step::Widen { operand, op } => match of(operand) {
                    Relation::Widened(widenings) => Relation::Widened(widenings.with(op)),
                    Relation::Unrelated => Relation::Unrelated,
                    Relation::Arithmetic(_) | Relation::NotAffine => Relation::NotAffine,
                },
                Step::Opaque => {
                    let inputs = followed.get(&id).map_or(&[][..], Vec::as_slice);
                    if inputs.iter().all(|&input| of(input) == Relation::Unrelated) {
                        Relation::Unrelated
                    } else {
                        Relation::NotAffine
                    }
                }
            };
            known.insert(id, Some(relation));
        }
        known
            .get(&value)
            .copied()
            .flatten()
            .unwrap_or(Relation::NotAffine)
    }

    /// How `value` relates to `widened`, which only widens it downstream.
    fn widened_from(&self, value: ValueId, mut widened: ValueId) -> Relation {
        let mut widenings = Widenings::NONE;
        while widened != value {
            match (
                self.node(widened).opcode,
                self.node(widened).operands.first(),
            ) {
                (OpcodeIr::Cast { op }, Some(Operand::Value(from)))
                    if WIDENING_CASTS.contains(&op) =>
                {
                    widenings = widenings.with(op);
                    widened = *from;
                }
                _ => return Relation::NotAffine,
            }
        }
        Relation::Widened(widenings)
    }

    /// How many steps back from `value` each value of `walk`'s chain lies:
    /// the fewest operands followed to reach it. A widening keeps the value,
    /// so stepping through one is no step, nor is a load back to what was
    /// stored; a truncation is one.
    fn depths(&self, walk: &Walk, value: ValueId) -> HashMap<ValueId, u32> {
        let operands = walk.followed();
        let mut depths = HashMap::new();
        let mut pending = VecDeque::from([(value, 0)]);
        while let Some((id, depth)) = pending.pop_front() {
            if depths.contains_key(&id) {
                continue;
            }
            depths.insert(id, depth);
            let widens = matches!(self.node(id).opcode,
                OpcodeIr::Cast { op } if WIDENING_CASTS.contains(&op))
                || self.node(id).opcode == OpcodeIr::Load;
            for &operand in operands.get(&id).into_iter().flatten() {
                if widens {
                    pending.push_front((operand, depth));
                } else {
                    pending.push_back((operand, depth + 1));
                }
            }
        }
        depths
    }

    /// `value`, and what it is only widened into downstream: a guard on
    /// those constrains it too. A truncation of it is left out: a guard on
    /// the narrow value says nothing about the wide one.
    fn downstream(&self, value: ValueId) -> BTreeSet<ValueId> {
        let mut values = BTreeSet::from([value]);
        let mut grew = true;
        while grew {
            grew = false;
            for (id, node) in self.body.values.iter().enumerate() {
                if let (OpcodeIr::Cast { op }, Some(Operand::Value(from))) =
                    (node.opcode, node.operands.first())
                    && WIDENING_CASTS.contains(&op)
                    && values.contains(from)
                {
                    grew |= values.insert(id as ValueId);
                }
            }
        }
        values
    }

    /// The stores the load `id` reads back: the non-volatile stores of its
    /// type to its address, syntactically, that run before it. `None` when
    /// there is none, or the load is volatile or atomic, or its type is not
    /// known: what it reads is then not traced.
    ///
    /// The nearest such store in the load's block overwrites every earlier
    /// one, so it alone is read back: proven when only operations that
    /// write no memory sit between them. Anything else between -- a call,
    /// any other store, an operation off that list -- may be what the load
    /// reads instead, so it is never skipped to reach a store. With none in
    /// the block, nothing is proven: the one store elsewhere, or the
    /// several, are followed with `causes` saying why.
    fn link(&self, id: ValueId) -> Option<Link> {
        let load = self.node(id);
        if load.opcode != OpcodeIr::Load || load.volatile {
            return None;
        }
        let (ty, address) = (load.ty?, load.operands.first()?);
        // A store's operands are the value it writes, then the address.
        let stores: Vec<ValueId> = (0..self.body.values.len() as ValueId)
            .filter(|&store| {
                let node = self.node(store);
                node.opcode == OpcodeIr::Store
                    && !node.volatile
                    && node.ty == Some(ty)
                    && node
                        .operands
                        .get(1)
                        .is_some_and(|stored_to| self.same_address(stored_to, address))
                    && self.runs_before(store, id)
            })
            .collect();
        let candidates: HashSet<ValueId> = stores.iter().copied().collect();
        let instrs = &self.body.blocks[load.block].instrs;
        let before = &instrs[..instrs
            .iter()
            .position(|&instr| instr == id)
            .unwrap_or(instrs.len())];
        let nearest = before.iter().rev().find(|instr| candidates.contains(instr));
        let (stores, causes) = match (nearest, &stores[..]) {
            (Some(&store), _) | (None, &[store]) => (vec![store], self.clobbers(store, id)),
            (None, []) => return None,
            // No store runs in the load's block before it, so anything
            // there that may write memory runs after every one of them.
            (None, _) => {
                let mut causes = self.writers(before);
                causes.insert(Unverified::SeveralStores);
                (stores, causes)
            }
        };
        Some(Link {
            stored: stores
                .iter()
                .filter_map(|&store| self.node(store).operands.first().cloned())
                .collect(),
            causes,
        })
    }

    /// What may change memory between `store` and `load`, which it runs
    /// before: anything but the operations that write none, or the blocks
    /// themselves when they differ.
    fn clobbers(&self, store: ValueId, load: ValueId) -> BTreeSet<Unverified> {
        let block = self.node(store).block;
        if block != self.node(load).block {
            return BTreeSet::from([Unverified::OtherBlock]);
        }
        let instrs = &self.body.blocks[block].instrs;
        let index = |id| instrs.iter().position(|&instr| instr == id);
        let (Some(from), Some(to)) = (index(store), index(load)) else {
            return BTreeSet::from([Unverified::OtherBlock]);
        };
        self.writers(&instrs[from + 1..to])
    }

    /// Why each of `instrs` may write memory: anything but the operations
    /// that write none.
    fn writers(&self, instrs: &[ValueId]) -> BTreeSet<Unverified> {
        instrs
            .iter()
            .filter_map(|&between| {
                let node = self.node(between);
                match node.opcode {
                    OpcodeIr::BinOp { .. }
                    | OpcodeIr::Cast { .. }
                    | OpcodeIr::ICmp
                    | OpcodeIr::Gep
                    | OpcodeIr::Select
                    | OpcodeIr::Phi => None,
                    OpcodeIr::Load if !node.volatile => None,
                    OpcodeIr::Call => Some(Unverified::CallBetween),
                    OpcodeIr::Store => Some(Unverified::StoreBetween),
                    _ => Some(Unverified::OperationBetween),
                }
            })
            .collect()
    }

    /// Whether addresses `a` and `b` are syntactically one: the same value,
    /// or two `getelementptr`s through one type over the same operands. A
    /// GEP's operands are compared one level deep, by value identity and
    /// constant; anything else -- a global, a cast, two loads -- is never
    /// matched, which only keeps a store from being forwarded.
    fn same_address(&self, a: &Operand, b: &Operand) -> bool {
        let (Operand::Value(a), Operand::Value(b)) = (a, b) else {
            return false;
        };
        if a == b {
            return true;
        }
        let (a, b) = (self.node(*a), self.node(*b));
        let identical = |x: &Operand, y: &Operand| match (x, y) {
            (Operand::Value(x), Operand::Value(y)) => x == y,
            (Operand::Const(x), Operand::Const(y)) => x == y,
            _ => false,
        };
        a.opcode == OpcodeIr::Gep
            && b.opcode == OpcodeIr::Gep
            && a.element.is_some()
            && a.element == b.element
            && a.operands.len() == b.operands.len()
            && a.operands
                .iter()
                .zip(&b.operands)
                .all(|(x, y)| identical(x, y))
    }

    /// Whether instruction `a` runs before `b` on every path to `b`.
    fn runs_before(&self, a: ValueId, b: ValueId) -> bool {
        let (block_a, block_b) = (self.node(a).block, self.node(b).block);
        if block_a != block_b {
            return self.doms.dominates(block_a, block_b);
        }
        let instrs = &self.body.blocks[block_a].instrs;
        let index = |id| instrs.iter().position(|&instr| instr == id);
        self.doms.dominates(block_a, block_b) && index(a) < index(b)
    }

    /// Which blocks `start` reaches, itself included, on paths that never
    /// enter `avoid`. Nothing when `start` is `avoid`.
    fn reachable_avoiding(&self, start: usize, avoid: usize) -> Vec<bool> {
        let blocks = &self.body.blocks;
        let mut reached = vec![false; blocks.len()];
        let mut pending = vec![start];
        while let Some(block) = pending.pop() {
            if block == avoid
                || block >= blocks.len()
                || std::mem::replace(&mut reached[block], true)
            {
                continue;
            }
            pending.extend(&blocks[block].successors);
        }
        reached
    }

    /// `phi` read as the `-O0` lowering of a min/max: two incoming values,
    /// chosen by a branch on a comparison of exactly those two. Clang emits
    /// no `select` at `-O0`, and `mem2reg` does not make one. The clamp,
    /// with the comparison it is made of.
    fn phi_clamp(&self, phi: &ValueNode) -> Option<(GuardSite, ValueId)> {
        let ([a, b], &[from_a, from_b]) = (phi.operands.as_slice(), phi.incoming.as_slice()) else {
            return None;
        };
        if a == b {
            return None;
        }
        // The nearest block above the `phi` that branches on such a
        // comparison. Every candidate dominates the `phi`, so they lie on
        // one chain of the dominator tree.
        let (compare, on_true) = (0..self.body.blocks.len())
            .filter(|&block| block != phi.block && self.doms.dominates(block, phi.block))
            .filter_map(|block| {
                let (compare, on_true, on_false) = self.branch_on(block)?;
                let [x, y] = self.node(compare).operands.as_slice() else {
                    return None;
                };
                if !((x == a && y == b) || (x == b && y == a)) {
                    return None;
                }
                // Which way out of `block` a value arriving from
                // `predecessor` left it: `true` when every path from `block`
                // to `predecessor` leaves by `on_true`. Dominance would not
                // do: a successor can be entered from elsewhere too.
                let from_true = self.reachable_avoiding(on_true, block);
                let from_false = self.reachable_avoiding(on_false, block);
                let side = |predecessor: usize| {
                    if predecessor == block {
                        return match (on_true == phi.block, on_false == phi.block) {
                            (true, false) => Some(true),
                            (false, true) => Some(false),
                            _ => None,
                        };
                    }
                    match (from_true[predecessor], from_false[predecessor]) {
                        (true, false) => Some(true),
                        (false, true) => Some(false),
                        _ => None,
                    }
                };
                let picked = match (side(from_a)?, side(from_b)?) {
                    (true, false) => a,
                    (false, true) => b,
                    _ => return None,
                };
                Some((block, compare, picked))
            })
            .reduce(|nearest, candidate| {
                if self.doms.dominates(nearest.0, candidate.0) {
                    candidate
                } else {
                    nearest
                }
            })
            .map(|(_, compare, picked)| (compare, picked))?;
        let node = self.node(compare);
        let predicate = int_predicate(node.guard.as_ref()?.predicate.as_deref()?)?;
        let picks = usize::from(node.operands.first() != Some(on_true));
        let clamp = predicate.clamps?[picks];
        let bound = [a, b].into_iter().find_map(|operand| match operand {
            Operand::Const(constant) => Some(unsigned_bound(
                GuardKind::Clamp,
                Some(clamp),
                &constant.value,
                constant.bits,
            )),
            _ => None,
        });
        let site = GuardSite {
            op: GuardKind::Clamp,
            predicate: Some(clamp.to_string()),
            bound,
            location: node.location.clone().or_else(|| phi.location.clone()),
            relation: GuardRelation::Defines,
            depth: 0,
            verified: true,
            caveat: None,
            affine: Some(Affine::IDENTITY),
        };
        Some((site, compare))
    }

    /// The comparison `block` ends by branching on, with the blocks it goes
    /// to when that is true and when false.
    fn branch_on(&self, block: usize) -> Option<(ValueId, usize, usize)> {
        let ir = &self.body.blocks[block];
        let terminator = self.node(*ir.instrs.last()?);
        let (OpcodeIr::Br, Some(Operand::Value(compare)), &[on_true, on_false]) = (
            terminator.opcode,
            terminator.operands.first(),
            ir.successors.as_slice(),
        ) else {
            return None;
        };
        (self.node(*compare).opcode == OpcodeIr::ICmp && on_true != on_false)
            .then_some((*compare, on_true, on_false))
    }

    /// The crossings into parameter `index` of the traced function: one per
    /// call site that calls it, bounded to the slice from `from` when given.
    fn crossings_into(&self, index: usize) -> Vec<Crossing> {
        calls_to(self.session, self.function)
            .into_iter()
            .filter(|site| {
                self.scope
                    .is_none_or(|scope| scope.contains(&site.id.function))
            })
            .map(|site| Crossing {
                site: site.id,
                direction: CrossingDir::Into,
                function: self.function.clone(),
                position: format!("parameter {index}"),
            })
            .collect()
    }

    /// The crossing out of the callee of call `id`, or why there is none.
    fn crossing_from(&self, id: ValueId) -> Result<Crossing, String> {
        let node = self.node(id);
        let missing = || {
            format!(
                "the call returning the value{} has no counterpart in the catalog",
                place(node)
            )
        };
        let site = catalog_site(self.body, self.session, self.function, id).ok_or_else(missing)?;
        match &self.session.call_site(&site).ok_or_else(missing)?.target {
            CallTarget::Direct { callee } => Ok(Crossing {
                site,
                direction: CrossingDir::From,
                function: definition_of(self.session, callee),
                position: RETURN_POSITION.to_string(),
            }),
            _ => Err(missing()),
        }
    }
}

/// The guard `raw` makes as one that produces the traced value, read from
/// its first operand among `operands` that is a value.
fn defining(raw: &GuardOpRaw, node: &ValueNode, operands: &[Operand]) -> GuardSite {
    let traced = operands
        .iter()
        .position(|operand| matches!(operand, Operand::Value(_)));
    surfaced(
        raw,
        traced == Some(1),
        node.location.clone(),
        GuardRelation::Defines,
    )
}

/// `raw` as a guard site, its predicate read with the traced value on the
/// left: swapped when the traced value is the right-hand operand.
fn surfaced(
    raw: &GuardOpRaw,
    traced_on_right: bool,
    location: Option<SourceLocation>,
    relation: GuardRelation,
) -> GuardSite {
    let predicate = raw.predicate.as_deref().map(|predicate| {
        if traced_on_right {
            read_from_right(raw.kind, predicate)
        } else {
            predicate
        }
    });
    GuardSite {
        op: raw.kind,
        bound: raw
            .bound
            .as_deref()
            .map(|bound| unsigned_bound(raw.kind, predicate, bound, raw.bits)),
        predicate: predicate.map(str::to_string),
        location,
        relation,
        depth: 0,
        verified: true,
        caveat: None,
        affine: Some(Affine::IDENTITY),
    }
}

/// `bound`, a constant as the IR prints it, read unsigned at width `bits`
/// when `op` with `predicate` is unsigned: a mask, or an operation whose
/// name starts with `u`. The IR prints every integer signed, so the mask
/// `0xfffffff0` reads `-16`. Kept as printed for a signed or equality
/// operation, when the width is not known, or when no such integer holds it.
fn unsigned_bound(
    op: GuardKind,
    predicate: Option<&str>,
    bound: &str,
    bits: Option<u32>,
) -> String {
    let unsigned = signedness(op, predicate) == Some(Signedness::Unsigned);
    bits.filter(|_| unsigned)
        .and_then(|bits| read_bound(bound, bits, false))
        .map_or_else(|| bound.to_string(), |value| value.to_string())
}

/// How `predicate` reads once its operands swap sides.
fn read_from_right(kind: GuardKind, predicate: &str) -> &str {
    match kind {
        GuardKind::Compare => int_predicate(predicate).map_or(predicate, |row| row.swapped),
        GuardKind::Saturate => REVERSED_SATURATES
            .iter()
            .find(|(forward, _)| *forward == predicate)
            .map_or(predicate, |(_, reversed)| reversed),
        // Min, max and masks commute; a remainder is a guard only when its
        // divisor, on the right, is the constant.
        GuardKind::Clamp | GuardKind::Mask | GuardKind::Modulo => predicate,
    }
}

/// ` at file:line` for `node`'s location, or nothing without one.
fn place(node: &ValueNode) -> String {
    node.location.as_ref().map_or_else(String::new, |location| {
        format!(" at {}:{}", location.file.display(), location.line)
    })
}

/// Intrinsic calls `mem2reg` deletes along with the stack slot they mark,
/// by name prefix. Left out wherever calls are counted, so a call's order
/// among those at its location is the same before and after promotion.
const PROMOTION_DELETES: &[&str] = &["llvm.lifetime."];

/// The SSA value a [`GuardAnchor`] names in the promoted module `ir`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// Value `value` of `ir.functions[function]`. `used_at` is the
    /// instruction a local read at a use-site is used at, when it was.
    One {
        function: usize,
        value: ValueId,
        used_at: Option<ValueId>,
    },
    /// A local of `ir.functions[function]` bound to more than one value over
    /// its lifetime: each SSA value it is bound to, in binding order. A
    /// binding to a constant, or to nothing traceable, counts toward the
    /// ambiguity but is not listed.
    Ambiguous {
        function: usize,
        values: Vec<ValueId>,
    },
}

/// The function `anchor` lies in: its call site's, or the one definition
/// a `local`'s function name names. ODR copies count as one definition.
pub(crate) fn anchor_function(
    session: &Session,
    anchor: &GuardAnchor,
) -> Result<FunctionId, Error> {
    let name = match anchor {
        GuardAnchor::Argument { site, .. } | GuardAnchor::Return { site } => {
            return Ok(site.function.clone());
        }
        GuardAnchor::Local { function, .. } => function,
    };
    one_definition(session, name, "a local")
}

/// The one function `name` defines, for `role` to name in the error when
/// it defines none or several. ODR copies count as one definition, and an
/// `available_externally` body as none.
pub(crate) fn one_definition(
    session: &Session,
    name: &str,
    role: &str,
) -> Result<FunctionId, Error> {
    let definitions = session
        .definitions(name)
        .into_iter()
        .filter(|function| function.linkage != Linkage::AvailableExternally)
        .map(|function| (function.linkage, function.id.clone()))
        .collect();
    match collapse_odr_duplicates(definitions).as_slice() {
        [] => Err(Error::InvalidArguments(format!(
            "no function named {name:?} is defined in the catalog"
        ))),
        [one] => Ok(one.clone()),
        several => Err(Error::InvalidArguments(format!(
            "{name:?} names {} defined functions ({}); {role} needs exactly one",
            several.len(),
            several
                .iter()
                .map(function_label)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The value `anchor` names in `ir`, the promoted module of
/// [`anchor_function`]'s module. A call site is found by its recorded source
/// location and its order among the calls sharing it, which promotion keeps:
/// `mem2reg` deletes slot accesses before a call, shifting its index, but
/// never the call itself.
pub(crate) fn resolve_anchor(
    ir: &ModuleIr,
    session: &Session,
    anchor: &GuardAnchor,
) -> Result<Resolved, Error> {
    resolve_anchor_at_use(ir, session, anchor, None)
}

/// [`resolve_anchor`], with a local read at the use on `at`, a `file:line`.
pub(crate) fn resolve_anchor_at_use(
    ir: &ModuleIr,
    session: &Session,
    anchor: &GuardAnchor,
    at: Option<&str>,
) -> Result<Resolved, Error> {
    let id = anchor_function(session, anchor)?;
    if at.is_some() && !matches!(anchor, GuardAnchor::Local { .. }) {
        return Err(Error::InvalidArguments(
            "a use-site picks one value of a local; a call site's argument or result is \
             already one value"
                .to_string(),
        ));
    }
    let function = ir
        .functions
        .iter()
        .position(|candidate| candidate.id.symbol == id.symbol)
        .ok_or_else(|| {
            Error::InvalidArguments(format!(
                "{} is not defined in its promoted module",
                function_label(&id)
            ))
        })?;
    let body = &ir.functions[function];
    match anchor {
        GuardAnchor::Argument { site, operand } => {
            let (_, call) = promoted_call(body, session, site)?;
            let refuse = |what: String| {
                Error::InvalidArguments(format!(
                    "operand {operand} of the call at {} {what}",
                    site_label(site)
                ))
            };
            match call.args.get(*operand) {
                Some(Operand::Value(value)) => Ok(Resolved::One {
                    function,
                    value: *value,
                    used_at: None,
                }),
                Some(Operand::Const(constant)) => Err(refuse(format!(
                    "is the constant {}, not a value to trace",
                    constant.value
                ))),
                Some(Operand::Func(callee)) => Err(refuse(format!(
                    "is the address of {}, not a value to trace",
                    callee.symbol
                ))),
                Some(Operand::Other) => Err(refuse(
                    "is not a value of the calling function: a global, a non-integer constant \
                     or poison"
                        .to_string(),
                )),
                None => Err(refuse(format!(
                    "is out of range: the call passes {} arguments",
                    call.args.len()
                ))),
            }
        }
        GuardAnchor::Return { site } => {
            let (value, call) = promoted_call(body, session, site)?;
            // Read from the call's type in the IR: a `void` call has no
            // result to trace.
            if call.returns_void {
                return Err(Error::InvalidArguments(format!(
                    "the call at {} returns no value: its callee returns void",
                    site_label(site)
                )));
            }
            Ok(Resolved::One {
                function,
                value,
                used_at: None,
            })
        }
        GuardAnchor::Local { name, .. } => {
            let at = at
                .map(|at| UseSite::new(session, &id.module_id, at))
                .transpose()?;
            resolve_local(body, &id, function, name, at.as_ref())
        }
    }
}

/// `site`'s call in `body`, its promoted function: the call with the same
/// order among those recorded at the site's location.
fn promoted_call<'a>(
    body: &'a FunctionIr,
    session: &Session,
    site: &CallSiteId,
) -> Result<(ValueId, &'a CallIr), Error> {
    let fact = session.call_site(site).ok_or_else(|| {
        Error::InvalidArguments(format!("no call {} in the catalog", site_label(site)))
    })?;
    let location = fact.location.as_ref();
    let missing = || {
        Error::InvalidArguments(format!(
            "the call at {} has no counterpart in the promoted module",
            site_label(site)
        ))
    };
    let ordinal = catalog_peers(session, &site.function, location)
        .iter()
        .position(|peer| *peer == site)
        .ok_or_else(missing)?;
    promoted_peers(body, location)
        .nth(ordinal)
        .ok_or_else(missing)
}

/// The catalog's call site for call `id` of `body`, the promoted `function`:
/// [`promoted_call`] the other way round.
fn catalog_site(
    body: &FunctionIr,
    session: &Session,
    function: &FunctionId,
    id: ValueId,
) -> Option<CallSiteId> {
    let location = body.values.get(id as usize)?.location.as_ref();
    let ordinal = promoted_peers(body, location).position(|(call, _)| call == id)?;
    catalog_peers(session, function, location)
        .get(ordinal)
        .map(|&peer| peer.clone())
}

/// The catalog's call sites in `function` recorded at `location`, in order,
/// without those promotion deletes.
fn catalog_peers<'s>(
    session: &'s Session,
    function: &FunctionId,
    location: Option<&SourceLocation>,
) -> Vec<&'s CallSiteId> {
    let mut peers: Vec<&CallSiteId> = session
        .call_sites()
        .iter()
        .filter(|peer| peer.id.function == *function)
        .filter(|peer| same_place(peer.location.as_ref(), location))
        .filter(|peer| match &peer.target {
            CallTarget::Intrinsic { name } => !deleted_by_promotion(name),
            _ => true,
        })
        .map(|peer| &peer.id)
        .collect();
    peers.sort_by_key(|peer| (peer.block_index, peer.instruction_index));
    peers
}

/// `body`'s calls at `location`, in order, without those promotion deletes.
fn promoted_peers<'a>(
    body: &'a FunctionIr,
    location: Option<&SourceLocation>,
) -> impl Iterator<Item = (ValueId, &'a CallIr)> {
    body.values
        .iter()
        .enumerate()
        .filter_map(|(id, node)| Some((id as ValueId, node, node.call.as_ref()?)))
        .filter(move |(_, node, _)| same_place(node.location.as_ref(), location))
        .filter(|(_, _, call)| {
            !call
                .callee
                .as_ref()
                .is_some_and(|callee| deleted_by_promotion(&callee.symbol))
        })
        .map(|(id, _, call)| (id, call))
}

/// The value the source variable `name` is bound to in `body`, which is
/// `ir.functions[function]` and the catalog's `id`. Bindings to one value are one value however
/// often they recur; bindings to several are reported, never chosen
/// between, unless `at` names a use: then the one value the local holds at
/// that use, refused when the line reads none or several.
fn resolve_local(
    body: &FunctionIr,
    id: &FunctionId,
    function: usize,
    name: &str,
    at: Option<&UseSite>,
) -> Result<Resolved, Error> {
    let mut bound: Vec<&Operand> = Vec::new();
    for binding in body
        .dbg_values
        .iter()
        .filter(|binding| binding.name == name)
    {
        if !bound.contains(&&binding.value) {
            bound.push(&binding.value);
        }
    }
    let label = function_label(id);
    if bound.is_empty() {
        return Err(Error::InvalidArguments(format!(
            "no debug binding of local `{name}` in {label}; resolving a local needs a module \
             built with -g"
        )));
    }
    let values: Vec<ValueId> = bound
        .iter()
        .filter_map(|operand| match operand {
            Operand::Value(value) => Some(*value),
            _ => None,
        })
        .collect();
    match (bound.len(), values.as_slice(), at) {
        (_, [], _) => {
            let constants: Vec<&str> = bound
                .iter()
                .filter_map(|operand| match operand {
                    Operand::Const(constant) => Some(constant.value.as_str()),
                    _ => None,
                })
                .collect();
            Err(Error::InvalidArguments(if constants.len() == bound.len() {
                format!(
                    "local `{name}` in {label} is bound only to the constant{} {}, not a value \
                     to trace",
                    if constants.len() == 1 { "" } else { "s" },
                    constants.join(", ")
                )
            } else {
                format!(
                    "local `{name}` in {label} is bound to no value to trace: it lives in \
                     memory, is computed from other values, or was optimized out"
                )
            }))
        }
        (_, _, Some(at)) => resolve_local_at(body, &label, function, name, at),
        (1, &[value], None) => Ok(Resolved::One {
            function,
            value,
            used_at: None,
        }),
        (_, _, None) => Ok(Resolved::Ambiguous { function, values }),
    }
}

/// The one value local `name` of `body`, the function `label` names, holds
/// at the uses on `at`.
fn resolve_local_at(
    body: &FunctionIr,
    label: &str,
    function: usize,
    name: &str,
    at: &UseSite,
) -> Result<Resolved, Error> {
    let doms = dominators(body);
    let uses = local_uses(body, &doms, name);
    let here: Vec<(ValueId, ValueId)> = uses
        .iter()
        .copied()
        .filter(|&(instr, _)| {
            body.values[instr as usize]
                .location
                .as_ref()
                .is_some_and(|location| at.holds(location))
        })
        .collect();
    let mut values: Vec<ValueId> = Vec::new();
    for &(_, value) in &here {
        if !values.contains(&value) {
            values.push(value);
        }
    }
    match values.as_slice() {
        [] => Err(Error::InvalidArguments(format!(
            "no use of `{name}` at {} in {label}: {}",
            at.spelled,
            match use_lines(body, &uses).as_slice() {
                [] => "no instruction with a recorded line reads it".to_string(),
                lines => format!("it is used at {}", lines.join(", ")),
            }
        ))),
        &[value] => {
            let instrs: Vec<ValueId> = here.iter().map(|&(instr, _)| instr).collect();
            Ok(Resolved::One {
                function,
                value,
                used_at: Some(shared_point(body, &doms, &instrs)),
            })
        }
        several => Err(Error::InvalidArguments(format!(
            "the uses of `{name}` in {label} at {} read more than one of its values ({} SSA \
             values), so none is picked",
            at.spelled,
            several.len()
        ))),
    }
}

/// A `file:line` naming the uses of a local, with each spelling of the file
/// the anchor's module recorded.
struct UseSite {
    /// As written, for errors.
    spelled: String,
    files: HashSet<std::path::PathBuf>,
    line: u32,
}

impl UseSite {
    /// `at` in the module `module_id`, matched as `at` matches a call site.
    fn new(session: &Session, module_id: &str, at: &str) -> Result<UseSite, Error> {
        let (file, line) = crate::parse_location(at.trim())?;
        let mut files: HashSet<std::path::PathBuf> = session
            .spellings_of(&file)
            .into_iter()
            .filter(|(module, _)| *module == module_id)
            .map(|(_, spelling)| spelling.to_path_buf())
            .collect();
        files.insert(file);
        Ok(UseSite {
            spelled: at.trim().to_string(),
            files,
            line,
        })
    }

    fn holds(&self, location: &SourceLocation) -> bool {
        location.line == self.line && self.files.contains(&location.file)
    }
}

/// Each instruction of `body` that reads the value local `name` holds
/// there, with that value. The value held is the one the last binding
/// before the instruction binds: in its block, or else the last in the
/// nearest block dominating it that has one. A `phi` merges values rather
/// than reading one, so it is no use; nor is any read where `name` holds a
/// constant or nothing traceable.
fn local_uses(body: &FunctionIr, doms: &Doms, name: &str) -> Vec<(ValueId, ValueId)> {
    let mut position = vec![0; body.values.len()];
    for block in &body.blocks {
        for (index, &id) in block.instrs.iter().enumerate() {
            position[id as usize] = index;
        }
    }
    // Each block's bindings of `name` in order, by the position each takes
    // effect before.
    let mut bindings: Vec<Vec<(usize, &Operand)>> = vec![Vec::new(); body.blocks.len()];
    for binding in body
        .dbg_values
        .iter()
        .filter(|binding| binding.name == name)
    {
        if let Some(node) = body.values.get(binding.at as usize)
            && let Some(block) = bindings.get_mut(node.block)
        {
            block.push((position[binding.at as usize], &binding.value));
        }
    }
    let held = |id: ValueId| {
        let mut block = body.values[id as usize].block;
        let mut binding = bindings[block]
            .iter()
            .rev()
            .find(|(at, _)| *at <= position[id as usize]);
        while binding.is_none() {
            match doms.idom.get(block) {
                Some(&Some(up)) if up != block => block = up,
                _ => return None,
            }
            binding = bindings[block].last();
        }
        match binding? {
            (_, Operand::Value(value)) => Some(*value),
            _ => None,
        }
    };
    body.values
        .iter()
        .enumerate()
        .filter(|(_, node)| !matches!(node.opcode, OpcodeIr::Param | OpcodeIr::Phi))
        .filter_map(|(id, node)| {
            let id = id as ValueId;
            let value = held(id)?;
            let operands = node.call.as_ref().map_or(&node.operands, |call| &call.args);
            operands
                .contains(&Operand::Value(value))
                .then_some((id, value))
        })
        .collect()
}

/// The `file:line` of each of `uses`, once each, in order.
fn use_lines(body: &FunctionIr, uses: &[(ValueId, ValueId)]) -> Vec<String> {
    let lines: BTreeSet<(String, u32)> = uses
        .iter()
        .filter_map(|&(instr, _)| body.values[instr as usize].location.as_ref())
        .filter(|location| location.line != 0)
        .map(|location| (location.file.display().to_string(), location.line))
        .collect();
    lines
        .into_iter()
        .map(|(file, line)| format!("{file}:{line}"))
        .collect()
}

/// The point every one of `instrs` runs after: the first of them in the
/// nearest block dominating them all, or that block's last instruction when
/// it holds none of them. Whatever runs before the point runs before each.
/// The first of them when no block dominates them all.
fn shared_point(body: &FunctionIr, doms: &Doms, instrs: &[ValueId]) -> ValueId {
    let first = instrs[0];
    let block_of = |id: ValueId| body.values[id as usize].block;
    let Some(shared) = instrs.iter().try_fold(block_of(first), |shared, &id| {
        doms.common_dominator(shared, block_of(id))
    }) else {
        return first;
    };
    let instrs_of_shared = &body.blocks[shared].instrs;
    instrs_of_shared
        .iter()
        .copied()
        .find(|id| instrs.contains(id))
        .or_else(|| instrs_of_shared.last().copied())
        .unwrap_or(first)
}

/// Whether two recorded locations name the same place. What is known about
/// the file now -- its source status -- is not part of the place.
fn same_place(a: Option<&SourceLocation>, b: Option<&SourceLocation>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.file == b.file
                && a.directory == b.directory
                && a.line == b.line
                && a.column == b.column
                && a.inlined_at.len() == b.inlined_at.len()
                && a.inlined_at
                    .iter()
                    .zip(&b.inlined_at)
                    .all(|(a, b)| same_place(Some(a), Some(b)))
        }
        _ => false,
    }
}

fn deleted_by_promotion(symbol: &str) -> bool {
    PROMOTION_DELETES
        .iter()
        .any(|prefix| symbol.starts_with(prefix))
}

/// Immediate dominators of a function's blocks, entry = block 0.
///
/// Blocks the entry cannot reach have no dominator information.
pub(crate) struct Doms {
    /// `idom[b]` is `b`'s immediate dominator; the entry is its own. `None`
    /// for a block unreachable from the entry.
    idom: Vec<Option<usize>>,
}

impl Doms {
    /// Whether every path from the entry to `b` passes through `a`. A block
    /// dominates itself. False for any pair involving a block unreachable
    /// from the entry (itself included) or an index out of range: nothing is
    /// claimed about code that cannot run.
    pub(crate) fn dominates(&self, a: usize, b: usize) -> bool {
        if !matches!(self.idom.get(a), Some(Some(_))) {
            return false;
        }
        let mut at = b;
        while let Some(Some(up)) = self.idom.get(at) {
            if at == a {
                return true;
            }
            if *up == at {
                return false;
            }
            at = *up;
        }
        false
    }

    /// The nearest block dominating both `a` and `b`. `None` when either
    /// is unreachable from the entry.
    fn common_dominator(&self, a: usize, b: usize) -> Option<usize> {
        let mut above_a = Vec::new();
        let mut at = a;
        loop {
            above_a.push(at);
            match self.idom.get(at) {
                Some(&Some(up)) if up != at => at = up,
                Some(&Some(_)) => break,
                _ => return None,
            }
        }
        let mut at = b;
        loop {
            if above_a.contains(&at) {
                return Some(at);
            }
            match self.idom.get(at) {
                Some(&Some(up)) if up != at => at = up,
                _ => return None,
            }
        }
    }
}

/// Cooper, Harvey and Kennedy's iterative algorithm over the owned CFG.
pub(crate) fn dominators(f: &FunctionIr) -> Doms {
    let n = f.blocks.len();
    if n == 0 {
        return Doms { idom: Vec::new() };
    }
    // Reverse postorder of the blocks reachable from the entry.
    let mut post = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    seen[0] = true;
    let mut stack = vec![(0usize, 0usize)];
    while let Some((block, next)) = stack.pop() {
        match f.blocks[block].successors.get(next) {
            Some(&succ) => {
                stack.push((block, next + 1));
                if succ < n && !seen[succ] {
                    seen[succ] = true;
                    stack.push((succ, 0));
                }
            }
            None => post.push(block),
        }
    }
    let mut rpo_number = vec![usize::MAX; n];
    for (i, &block) in post.iter().rev().enumerate() {
        rpo_number[block] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &block in &post {
        for &succ in &f.blocks[block].successors {
            if succ < n {
                preds[succ].push(block);
            }
        }
    }

    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[0] = Some(0);
    let mut changed = true;
    while changed {
        changed = false;
        for &block in post.iter().rev().skip(1) {
            let mut new_idom: Option<usize> = None;
            for &pred in &preds[block] {
                if idom[pred].is_none() {
                    continue;
                }
                new_idom = Some(match new_idom {
                    None => pred,
                    Some(cur) => intersect(&idom, &rpo_number, pred, cur),
                });
            }
            if new_idom.is_some() && idom[block] != new_idom {
                idom[block] = new_idom;
                changed = true;
            }
        }
    }
    Doms { idom }
}

/// The nearest common dominator of `a` and `b` among those found so far.
fn intersect(idom: &[Option<usize>], rpo_number: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rpo_number[a] > rpo_number[b] {
            a = idom[a].expect("processed block has an idom");
        }
        while rpo_number[b] > rpo_number[a] {
            b = idom[b].expect("processed block has an idom");
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallTarget, FunctionId,
        extract::read_module_ir,
        ir::{BinaryOp, BlockIr, OpcodeIr, Operand, ScalarKind, TypeTag, ValueNode},
        slice::promote_anchor_module,
        testing::{compiled_session, module_session, stale_debug_info_module},
    };

    /// A function whose block `i` branches to `successors[i]`.
    fn cfg(successors: &[&[usize]]) -> FunctionIr {
        FunctionIr {
            id: FunctionId {
                module_id: "m".into(),
                symbol: "f".into(),
            },
            params: Vec::new(),
            blocks: successors
                .iter()
                .map(|s| BlockIr {
                    instrs: Vec::new(),
                    successors: s.to_vec(),
                })
                .collect(),
            values: Vec::new(),
            dbg_values: Vec::new(),
        }
    }

    /// Two calls whose arguments are a parameter, a clamp and a constant.
    const ARGUMENT_SOURCE: &str = "extern void sink(unsigned, unsigned);
void f(unsigned len) {
  unsigned n = len < 16 ? len : 16;
  sink(len, n);
  sink(n, 7);
}
";

    const RETURN_SOURCE: &str = "extern unsigned get(void);
extern void sink(unsigned);
void f(void) { unsigned n = get(); sink(n); }
";

    /// `a` is bound once; `x` is rebound to a second value.
    const LOCAL_SOURCE: &str = "extern void sink(unsigned);
void g(unsigned a) {
  unsigned x = a;
  sink(x);
  x = a + 1;
  sink(x);
}
";

    /// `cur` holds `a`, then `b`: each guarded on line 4 or 7 and used on
    /// line 5 or 8.
    const REUSED_LOCAL_SOURCE: &str = "extern void sink(unsigned);
void g(unsigned a, unsigned b) {
  unsigned cur = a;
  if (cur > 64) return;
  sink(cur);
  cur = b;
  if (cur > 16) return;
  sink(cur);
}
";

    /// Line 6 reads `keep`, which holds `cur`'s first value, beside `cur`,
    /// which by then holds its second.
    const KEPT_VALUE_SOURCE: &str = "extern void sink(unsigned, unsigned);
void g(unsigned a, unsigned b) {
  unsigned cur = a;
  unsigned keep = cur;
  cur = b;
  sink(keep, cur);
}
";

    /// Line 4 uses `cur` before and after it is rebound.
    const ONE_LINE_REBINDING_SOURCE: &str = "extern void sink(unsigned);
void g(unsigned a, unsigned b) {
  unsigned cur = a;
  sink(cur); cur = b; sink(cur);
}
";

    /// `cur` is rebound on one arm only, so line 5 reads the `phi` that
    /// joins the two values.
    const JOINED_LOCAL_SOURCE: &str = "extern void sink(unsigned);
void g(unsigned a, unsigned b, int c) {
  unsigned cur = a;
  if (c) cur = b;
  sink(cur);
}
";

    /// Both calls expand from one macro use, so they share a source
    /// location; the stack slots before them are promoted away.
    const SHARED_LOCATION_SOURCE: &str = "extern void sink(unsigned);
#define BOTH(p, q) sink(p); sink(q)
void f(unsigned a, unsigned b) {
  unsigned x = a + 1;
  unsigned y = b * 2;
  BOTH(x, y);
}
";

    /// `source` built with `flags` as module `m`, its session, and the
    /// promoted IR of `function`'s module.
    fn promoted(
        scratch: &tempfile::TempDir,
        source: &str,
        flags: &[&str],
        function: &str,
    ) -> (Session, ModuleIr) {
        let session = compiled_session(scratch.path(), "t.c", source, flags);
        let anchor = FunctionId {
            module_id: "m".into(),
            symbol: function.into(),
        };
        let bytes = promote_anchor_module(&session, &anchor).unwrap().bytes;
        (session, read_module_ir(&bytes).unwrap())
    }

    /// The catalog's sites in `function` that call `callee`, in order.
    fn sites_calling(session: &Session, function: &str, callee: &str) -> Vec<CallSiteId> {
        session
            .call_sites()
            .iter()
            .filter(|site| site.id.function.symbol == function)
            .filter(|site| {
                matches!(&site.target, CallTarget::Direct { callee: c } if c.symbol == callee)
            })
            .map(|site| site.id.clone())
            .collect()
    }

    /// The node `resolved` names, which must be a single value of `symbol`.
    fn node<'a>(ir: &'a ModuleIr, resolved: &Resolved, symbol: &str) -> (ValueId, &'a ValueNode) {
        let Resolved::One {
            function, value, ..
        } = *resolved
        else {
            panic!("expected one value, got {resolved:?}");
        };
        let function = &ir.functions[function];
        assert_eq!(function.id.symbol, symbol);
        (value, &function.values[value as usize])
    }

    fn defined<'a>(ir: &'a ModuleIr, symbol: &str) -> &'a FunctionIr {
        ir.functions.iter().find(|f| f.id.symbol == symbol).unwrap()
    }

    fn argument(site: &CallSiteId, operand: usize) -> GuardAnchor {
        GuardAnchor::Argument {
            site: site.clone(),
            operand,
        }
    }

    fn local(function: &str, name: &str) -> GuardAnchor {
        GuardAnchor::Local {
            function: function.into(),
            name: name.into(),
        }
    }

    /// An argument resolves to the value the call passes, and one that is
    /// no value -- a constant, or an operand past the last -- is refused
    /// naming it.
    #[test]
    fn an_argument_resolves_to_its_value_or_is_refused_naming_it() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, ARGUMENT_SOURCE, &["-O0", "-g"], "f");
        let sites = sites_calling(&session, "f", "sink");
        let f = defined(&ir, "f");

        let len = resolve_anchor(&ir, &session, &argument(&sites[0], 0)).unwrap();
        assert_eq!(node(&ir, &len, "f").0, f.params[0]);

        let n = resolve_anchor(&ir, &session, &argument(&sites[0], 1)).unwrap();
        assert_eq!(node(&ir, &n, "f").1.opcode, OpcodeIr::Phi);
        let again = resolve_anchor(&ir, &session, &argument(&sites[1], 0)).unwrap();
        assert_eq!(again, n, "both calls pass the one clamped value");

        let error = resolve_anchor(&ir, &session, &argument(&sites[1], 1))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("operand 1") && error.contains("constant 7"),
            "{error}"
        );
        let error = resolve_anchor(&ir, &session, &argument(&sites[0], 2))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("operand 2") && error.contains("2 arguments"),
            "{error}"
        );
    }

    #[test]
    fn a_return_resolves_to_the_call_result() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, RETURN_SOURCE, &["-O0", "-g"], "f");
        let get = sites_calling(&session, "f", "get");

        let resolved = resolve_anchor(
            &ir,
            &session,
            &GuardAnchor::Return {
                site: get[0].clone(),
            },
        )
        .unwrap();
        let (value, call) = node(&ir, &resolved, "f");
        assert_eq!(
            call.call
                .as_ref()
                .and_then(|c| c.callee.as_ref())
                .map(|c| c.symbol.as_str()),
            Some("get")
        );
        let f = defined(&ir, "f");
        let sink = f
            .values
            .iter()
            .filter_map(|v| v.call.as_ref())
            .find(|c| c.callee.as_ref().is_some_and(|c| c.symbol == "sink"))
            .unwrap();
        assert_eq!(
            sink.args,
            [Operand::Value(value)],
            "the result reaches sink"
        );
    }

    #[test]
    fn a_local_resolves_through_its_debug_bindings() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, LOCAL_SOURCE, &["-O0", "-g"], "g");
        let g = defined(&ir, "g");

        let a = resolve_anchor(&ir, &session, &local("g", "a")).unwrap();
        assert_eq!(node(&ir, &a, "g").0, g.params[0]);

        // `x` holds `a`, then `a + 1`: two live values, reported as such.
        let Resolved::Ambiguous { values, .. } =
            resolve_anchor(&ir, &session, &local("g", "x")).unwrap()
        else {
            panic!("`x` is bound to two values");
        };
        assert_eq!(values.len(), 2, "{values:?}");
        assert_eq!(values[0], g.params[0]);
        assert_eq!(
            g.values[values[1] as usize].opcode,
            OpcodeIr::BinOp { op: BinaryOp::Add }
        );
    }

    #[test]
    fn a_local_without_debug_info_is_refused_naming_it() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, LOCAL_SOURCE, &["-O0"], "g");
        let error = resolve_anchor(&ir, &session, &local("g", "x"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`x`") && error.contains("-g"), "{error}");
    }

    /// `file:line` in `t.c` under `scratch`.
    fn line_of(scratch: &tempfile::TempDir, line: u32) -> String {
        format!("{}:{line}", scratch.path().join("t.c").display())
    }

    #[test]
    fn a_reused_local_resolves_to_the_value_used_at_a_line() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, REUSED_LOCAL_SOURCE, &["-O0", "-g"], "g");
        let g = defined(&ir, "g");
        let cur = local("g", "cur");

        let first =
            resolve_anchor_at_use(&ir, &session, &cur, Some(&line_of(&scratch, 5))).unwrap();
        assert_eq!(node(&ir, &first, "g").0, g.params[0]);
        let second =
            resolve_anchor_at_use(&ir, &session, &cur, Some(&line_of(&scratch, 8))).unwrap();
        assert_eq!(node(&ir, &second, "g").0, g.params[1]);
    }

    /// `keep` still holds `a` on line 6, but `cur` holds `b` there: the
    /// value a line uses counts only while the local is bound to it.
    #[test]
    fn a_use_site_picks_the_value_the_local_holds_there() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, KEPT_VALUE_SOURCE, &["-O0", "-g"], "g");
        let g = defined(&ir, "g");
        let resolved = resolve_anchor_at_use(
            &ir,
            &session,
            &local("g", "cur"),
            Some(&line_of(&scratch, 6)),
        )
        .unwrap();
        assert_eq!(node(&ir, &resolved, "g").0, g.params[1]);
    }

    /// Pins how `mem2reg` binds a local at a join: to the `phi` it creates,
    /// in the join block. Were that binding gone, the nearest one above
    /// would bind `a`, which line 5 does not read.
    #[test]
    fn a_use_after_a_join_reads_the_phi_of_the_values_joined() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, JOINED_LOCAL_SOURCE, &["-O0", "-g"], "g");
        let resolved = resolve_anchor_at_use(
            &ir,
            &session,
            &local("g", "cur"),
            Some(&line_of(&scratch, 5)),
        )
        .unwrap();
        let (_, phi) = node(&ir, &resolved, "g");
        assert_eq!(phi.opcode, OpcodeIr::Phi);
        let g = defined(&ir, "g");
        assert!(
            phi.operands.contains(&Operand::Value(g.params[0]))
                && phi.operands.contains(&Operand::Value(g.params[1])),
            "{phi:?}"
        );
    }

    #[test]
    fn a_use_site_with_no_use_of_the_local_is_refused_listing_its_use_lines() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, REUSED_LOCAL_SOURCE, &["-O0", "-g"], "g");
        let at = line_of(&scratch, 99);
        let error = resolve_anchor_at_use(&ir, &session, &local("g", "cur"), Some(&at))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("no use of `cur` at {at} in g [m]")),
            "{error}"
        );
        assert!(
            error.contains("t.c:5") && error.contains("t.c:8"),
            "{error}"
        );
    }

    #[test]
    fn a_use_site_reading_two_values_of_the_local_is_refused_not_picked() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, ONE_LINE_REBINDING_SOURCE, &["-O0", "-g"], "g");
        let error = resolve_anchor_at_use(
            &ir,
            &session,
            &local("g", "cur"),
            Some(&line_of(&scratch, 4)),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`cur`") && error.contains("more than one of its values"),
            "{error}"
        );
    }

    #[test]
    fn a_reused_local_without_a_use_site_is_reported_with_its_use_lines() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, REUSED_LOCAL_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &local("g", "cur"), None).unwrap();
        assert!(r.guards.is_empty(), "{r:?}");
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(lost.contains("bound to more than one value"), "{lost}");
        assert!(lost.contains("t.c:5") && lost.contains("t.c:8"), "{lost}");
    }

    /// Each use-site traces its own value: the bound checked before it,
    /// which runs before that use on every path.
    #[test]
    fn a_reused_local_at_a_use_site_reports_that_values_guards() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, REUSED_LOCAL_SOURCE, &["-O0", "-g"]);
        let cur = local("g", "cur");
        let first = guards_at_use(&session, &cur, Some(&line_of(&scratch, 5)), None).unwrap();
        assert_eq!(
            sites(&first),
            [(
                GuardKind::Compare,
                Some("ugt"),
                Some("64"),
                GuardRelation::Dominates
            )],
            "{first:?}"
        );
        let second = guards_at_use(&session, &cur, Some(&line_of(&scratch, 8)), None).unwrap();
        assert_eq!(
            sites(&second),
            [(
                GuardKind::Compare,
                Some("ugt"),
                Some("16"),
                GuardRelation::Dominates
            )],
            "{second:?}"
        );
        assert!(first.lost.is_none() && second.lost.is_none());
    }

    /// Mapping by position would land on another instruction: the promoted
    /// module has lost every slot access before the calls. The calls keep
    /// their order among those sharing a location.
    #[test]
    fn an_argument_after_promoted_slots_resolves_by_call_ordinal() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, SHARED_LOCATION_SOURCE, &["-O0", "-g"], "f");
        let sites = sites_calling(&session, "f", "sink");
        assert_eq!(sites.len(), 2);
        let location = |id: &CallSiteId| session.call_site(id).unwrap().location.clone();
        assert!(location(&sites[0]).is_some());
        assert_eq!(location(&sites[0]), location(&sites[1]), "one macro use");

        let f = defined(&ir, "f");
        let by_index = |site: &CallSiteId| {
            f.blocks[site.block_index as usize]
                .instrs
                .get(site.instruction_index as usize)
                .map(|&id| f.values[id as usize].opcode)
        };
        assert_ne!(by_index(&sites[1]), Some(OpcodeIr::Call));

        let x = resolve_anchor(&ir, &session, &argument(&sites[0], 0)).unwrap();
        assert_eq!(
            node(&ir, &x, "f").1.opcode,
            OpcodeIr::BinOp { op: BinaryOp::Add }
        );
        let y = resolve_anchor(&ir, &session, &argument(&sites[1], 0)).unwrap();
        assert_eq!(
            node(&ir, &y, "f").1.opcode,
            OpcodeIr::BinOp { op: BinaryOp::Mul }
        );
    }

    /// `n` is `len` clamped to `CAP`, then passed on.
    const CLAMP_SOURCE: &str = "#define CAP 16
extern void sink(unsigned);
void f(unsigned len) {
  unsigned n = len < CAP ? len : CAP;
  sink(n);
}
";

    const DOMINATING_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) {
  if (len > 16)
    return;
  sink(len);
}
";

    /// The constant is on the left of the comparison as written.
    const BOUND_ON_LEFT_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) {
  if (16 > len)
    sink(len);
}
";

    /// The comparison runs only when `flag` is set, so it does not dominate
    /// the call; nor does the one after the call.
    const ON_PATH_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len, int flag) {
  if (flag) {
    if (len > 16)
      return;
  }
  sink(len);
  if (len < 4)
    sink(0);
}
";

    const UNGUARDED_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) { sink(len); }
";

    /// The clamped value is mixed with an integer that went through a
    /// pointer.
    const ROUND_TRIP_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len, char *p) {
  unsigned n = len < 16 ? len : 16;
  sink(n + (unsigned)(__UINTPTR_TYPE__)p);
}
";

    /// `callee`'s parameter is passed by two callers; `lone` calls nothing.
    const CROSSING_SOURCE: &str = "extern void sink(unsigned);
void callee(unsigned n) { sink(n); }
void caller(unsigned x) { callee(x); }
void other(unsigned y) { callee(y); }
void lone(void) {}
";

    /// `len` reaches `n` past the comparison either way when `flag` is set:
    /// the branch on `len < 16` does not decide which value `n` takes.
    const GOTO_SOURCE: &str = "extern void sink(unsigned);
extern void side(void);
void f(unsigned len, int flag) {
  unsigned n;
  if (len < 16) goto A;
  side();
  if (flag) goto A;
  n = 16;
  goto done;
A:
  n = len;
done:
  sink(n);
}
";

    /// A choice between `len` and the bound on something else entirely.
    const UNRELATED_CHOICE_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len, int flag) {
  unsigned n = flag ? len : 16;
  sink(n);
}
";

    /// The call's result is compared after the call, and clamped.
    const RESULT_SOURCE: &str = "extern unsigned get(void);
extern void sink(unsigned);
void f(void) {
  unsigned r = get();
  if (r > 100)
    return;
  unsigned n = r < 16 ? r : 16;
  sink(n);
}
";

    /// `n` is `len` clamped to 16, computed from the anchored `len`; the
    /// clamp's comparison also decides the edge into the call passing `len`.
    const DOWNSTREAM_CLAMP_DECIDING_SOURCE: &str = "extern void sink(unsigned);
extern void keep(unsigned);
void f(unsigned len) {
  unsigned n;
  if (len < 16) {
    n = len;
    sink(len);
  } else {
    n = 16;
  }
  keep(n);
}
";

    /// The same clamp, whose comparison decides no edge into the call: both
    /// of its arms join before it.
    const DOWNSTREAM_CLAMP_JOINED_SOURCE: &str = "extern void sink(unsigned);
extern void keep(unsigned);
void f(unsigned len) {
  unsigned n = len < 16 ? len : 16;
  sink(len);
  keep(n);
}
";

    /// Results read from a register and from memory by intrinsics.
    const READING_INTRINSIC_IR: &str = r#"
declare i64 @llvm.read_register.i64(metadata)
declare <2 x i32> @llvm.masked.load.v2i32.p0(ptr, <2 x i1>, <2 x i32>)
declare void @sink(i64)
declare void @sinkv(<2 x i32>)

define void @f(ptr %p) {
  %v = call i64 @llvm.read_register.i64(metadata !0)
  call void @sink(i64 %v)
  %w = call <2 x i32> @llvm.masked.load.v2i32.p0(ptr align 4 %p, <2 x i1> <i1 true, i1 true>, <2 x i32> zeroinitializer)
  call void @sinkv(<2 x i32> %w)
  ret void
}

!0 = !{!"sp"}
"#;

    /// A target intrinsic that loads through its pointer argument, which
    /// comes from a parameter `caller` passes.
    const TARGET_LOAD_IR: &str = r#"
declare i64 @llvm.aarch64.ldxr.p0(ptr)
declare void @sink(i64)

define void @h(ptr %p) {
  %v = call i64 @llvm.aarch64.ldxr.p0(ptr elementtype(i64) %p)
  call void @sink(i64 %v)
  ret void
}

define void @caller(ptr %q) {
  call void @h(ptr %q)
  ret void
}
"#;

    /// The mask bounds `len & 0xff`, which `n` is computed from; `n` itself
    /// lies in [1000, 1255].
    const MASKED_OPERAND_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) {
  unsigned n = (len & 0xff) + 1000;
  sink(n);
}
";

    /// The comparison bounds `len`; `n` is `len * 4`.
    const COMPARED_OPERAND_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) {
  unsigned n = len * 4;
  if (len > 16)
    return;
  sink(n);
}
";

    /// `len = -2^32 + 100` passes the comparison and truncates to 100: the
    /// bound on `len` is not one on `(int)len`.
    const TRUNCATED_SOURCE: &str = "extern void sink(int);
void f(long len) {
  if (len > 64)
    return;
  sink((int)len);
}
";

    /// A bound the narrowed `unsigned char` cannot even reach.
    const NARROWED_SOURCE: &str = "extern void sink(unsigned char);
void f(unsigned len) {
  if (len < 300)
    return;
  sink((unsigned char)len);
}
";

    /// A widening keeps the value, so its bound holds for the anchor.
    const WIDENED_SOURCE: &str = "extern void sink(unsigned long);
void f(unsigned len) {
  if (len > 64)
    return;
  sink((unsigned long)len);
}
";

    /// A mask and a comparison whose constants the IR prints negative.
    const UNSIGNED_BOUNDS_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) {
  if (len < 0x80000000u)
    sink(len & 0xfffffff0u);
}
";

    /// `use` is called from two functions named `parse`.
    const TWO_PARSERS_SOURCE: &str = "extern \"C\" void sink(unsigned);
void use(unsigned n) { sink(n); }
namespace a { void parse(unsigned x) { use(x); } }
namespace b { void parse(unsigned x) { use(x); } }
";

    /// The value comes out of an `asm goto`.
    const ASM_GOTO_SOURCE: &str = "extern void sink(unsigned);
void f(void) {
  unsigned v;
  asm goto(\"\" : \"=r\"(v) : : : out);
  sink(v);
out:
  return;
}
";

    const VOID_RETURN_SOURCE: &str = "extern void act(void);
void f(void) { act(); }
";

    /// The lifetime markers of `t`'s slot share the macro's location with
    /// both calls.
    const LIFETIME_SOURCE: &str = "extern unsigned get(void);
extern void sink(unsigned);
#define M() do { unsigned t = get(); sink(t); } while (0)
void f(void) { M(); }
";

    /// One function per affine rule, each passing `sink` or `sink64` a
    /// value computed from `%x`, which one comparison bounds.
    const AFFINE_IR: &str = r#"
declare void @sink(i32)
declare void @sink64(i64)
declare void @sink8(i8)
declare i32 @llvm.umin.i32(i32, i32)

define void @offset(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @scaled(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = mul i32 4, %x
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @reversed(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = sub i32 10, %x
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @composed(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %a = mul i32 %x, 3
  %b = sub i32 %a, 5
  %n = shl i32 %b, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @overshifted(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = shl i32 %x, 32
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @overflowing(i64 %x) {
  %c = icmp ult i64 %x, 16
  br i1 %c, label %use, label %out
use:
  %a = mul i64 %x, 4611686018427387904
  %b = mul i64 %a, 4611686018427387904
  %n = mul i64 %b, 4611686018427387904
  call void @sink64(i64 %n)
  br label %out
out:
  ret void
}

define void @divergent(i32 %x, i1 %flag) {
  %c = icmp ult i32 %x, 16
  br i1 %flag, label %one, label %two
one:
  %a = add i32 %x, 1
  br label %join
two:
  %b = add i32 %x, 2
  br label %join
join:
  %n = phi i32 [ %a, %one ], [ %b, %two ]
  call void @sink(i32 %n)
  ret void
}

define void @agreeing(i32 %x, i1 %flag) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %body, label %out
body:
  br i1 %flag, label %one, label %two
out:
  ret void
one:
  %a = add i32 %x, 1
  br label %join
two:
  %b = add i32 1, %x
  br label %join
join:
  %n = phi i32 [ %a, %one ], [ %b, %two ]
  call void @sink(i32 %n)
  ret void
}

define void @partial(i32 %x, i1 %flag) {
  %c = icmp ult i32 %x, 16
  br i1 %flag, label %one, label %join
one:
  br label %join
join:
  %n = phi i32 [ %x, %one ], [ 7, %0 ]
  call void @sink(i32 %n)
  ret void
}

define void @widened(i32 %x) {
  %c = icmp ult i32 %x, 16
  %n = zext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @signed_widened(i32 %x) {
  %c = icmp slt i32 %x, 16
  %n = sext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @mismatched(i32 %x) {
  %c = icmp ult i32 %x, 16
  %n = sext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @widened_then_added(i32 %x) {
  %c = icmp ult i32 %x, 16
  %w = zext i32 %x to i64
  %n = add i64 %w, 1
  call void @sink64(i64 %n)
  ret void
}

define void @added_then_widened(i32 %x) {
  %c = icmp ult i32 %x, 16
  %a = add i32 %x, 1
  %n = zext i32 %a to i64
  call void @sink64(i64 %n)
  ret void
}

define void @truncated(i64 %x) {
  %c = icmp ult i64 %x, 16
  %n = trunc i64 %x to i32
  call void @sink(i32 %n)
  ret void
}

define void @summed(i32 %x, i32 %y) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = add i32 %x, %y
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @wrapping(i8 %x) {
  %c = icmp ule i8 %x, -2
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 2
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @unsigned_negative(i8 %x) {
  %c = icmp ult i8 %x, -16
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 10
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @unsigned_negative_wrapping(i8 %x) {
  %c = icmp ult i8 %x, -16
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 20
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @carrying(i8 %x) {
  %c = icmp ule i8 %x, 100
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 2
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @above(i32 %x) {
  %c = icmp ugt i32 %x, 16
  br i1 %c, label %out, label %use
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @signed_offset(i8 %x) {
  %c = icmp slt i8 %x, 16
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 1
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @signed_wrapping(i8 %x) {
  %c = icmp sgt i8 %x, 100
  br i1 %c, label %use, label %out
use:
  %n = add i8 %x, 100
  call void @sink8(i8 %n)
  br label %out
out:
  ret void
}

define void @false_edge(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %out, label %use
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @shared_successor(i32 %x) {
  %c = icmp ult i32 %x, 16
  br i1 %c, label %other, label %join
other:
  br label %join
join:
  br label %use
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  ret void
}

define void @never_above(i32 %x) {
  %c = icmp ugt i32 %x, -1
  br i1 %c, label %use, label %out
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @reversed_within(i32 %x) {
  %c = icmp ule i32 %x, 10
  br i1 %c, label %use, label %out
use:
  %n = sub i32 10, %x
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @unbounded(i32 %x, i32 %y) {
  %c = icmp ugt i32 %x, %y
  br i1 %c, label %out, label %use
use:
  %n = add i32 %x, 1
  call void @sink(i32 %n)
  br label %out
out:
  ret void
}

define void @clamped_to_value(i32 %x, i32 %y) {
  %m = call i32 @llvm.umin.i32(i32 %x, i32 %y)
  %n = add i32 %m, 1
  call void @sink(i32 %n)
  ret void
}

define void @clamped_then_added(i32 %x) {
  %m = call i32 @llvm.umin.i32(i32 %x, i32 16)
  %n = add i32 %m, 1
  call void @sink(i32 %n)
  ret void
}

define void @unsigned_itself(i32 %x) {
  %c = icmp ugt i32 %x, 16
  br i1 %c, label %out, label %use
use:
  call void @sink(i32 %x)
  br label %out
out:
  ret void
}

define void @signed_itself(i32 %x) {
  %c = icmp slt i32 %x, -16
  br i1 %c, label %use, label %out
use:
  call void @sink(i32 %x)
  br label %out
out:
  ret void
}

define void @equal_itself(i32 %x) {
  %c = icmp eq i32 %x, -1
  br i1 %c, label %use, label %out
use:
  call void @sink(i32 %x)
  br label %out
out:
  ret void
}

define void @equal_widened(i32 %x) {
  %c = icmp eq i32 %x, 5
  %n = zext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @negative_equal_widened(i32 %x) {
  %c = icmp eq i32 %x, -1
  %n = zext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @negative_equal_sign_extended(i32 %x) {
  %c = icmp eq i32 %x, -1
  %n = sext i32 %x to i64
  call void @sink64(i64 %n)
  ret void
}

define void @flag(i32 %x) {
  %c = icmp ult i32 %x, 16
  %n = zext i1 %c to i32
  call void @sink(i32 %n)
  ret void
}
"#;

    /// A saturating subtraction from the bound: `16 - len`, floored at 0.
    const SATURATE_SOURCE: &str = "extern void sink(unsigned);
void f(unsigned len) { sink(__builtin_elementwise_sub_sat(16u, len)); }
";

    /// `source` compiled with `flags` as the one module of a session.
    fn session(scratch: &tempfile::TempDir, source: &str, flags: &[&str]) -> Session {
        compiled_session(scratch.path(), "t.c", source, flags)
    }

    /// Operand 0 of `function`'s `n`th call to `sink`.
    fn sink_argument(session: &Session, function: &str, n: usize) -> GuardAnchor {
        argument(&sites_calling(session, function, "sink")[n], 0)
    }

    /// Each guard as (op, predicate, bound, relation).
    fn sites(result: &GuardsResult) -> Vec<(GuardKind, Option<&str>, Option<&str>, GuardRelation)> {
        result
            .guards
            .iter()
            .map(|guard| {
                (
                    guard.op,
                    guard.predicate.as_deref(),
                    guard.bound.as_deref(),
                    guard.relation,
                )
            })
            .collect()
    }

    /// Each crossing as (calling function, direction, function, position).
    fn crossings(result: &GuardsResult) -> Vec<(&str, CrossingDir, &str, &str)> {
        result
            .crossings
            .iter()
            .map(|crossing| {
                (
                    crossing.site.function.symbol.as_str(),
                    crossing.direction,
                    crossing.function.symbol.as_str(),
                    crossing.position.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn a_min_clamp_is_surfaced_as_a_defining_guard() {
        for optimization in ["-O0", "-O1"] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, CLAMP_SOURCE, &[optimization, "-g"]);
            let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
            // The clamp reports the comparison it is made of: it is not
            // listed again.
            assert_eq!(
                sites(&r),
                [(
                    GuardKind::Clamp,
                    Some("umin"),
                    Some("16"),
                    GuardRelation::Defines
                )],
                "{optimization}: {r:?}"
            );
            assert!(
                r.guards[0].location.as_ref().is_some_and(|at| at.line == 4),
                "{optimization}: {r:?}"
            );
            assert!(r.lost.is_none(), "{optimization}: {r:?}");
            // A guard on the anchor itself is at depth zero.
            assert!(
                r.guards.iter().all(|guard| guard.depth == 0),
                "{optimization}: {r:?}"
            );
            // The anchor is the clamp's result: its bound is the anchor's.
            assert!(r.guards[0].verified, "{optimization}: {r:?}");
            assert_eq!(r.guards[0].affine, Some(Affine::IDENTITY));
        }
    }

    /// The comparison dominates the call that passes `len`, and is on the
    /// path of the local `len` itself.
    #[test]
    fn a_dominating_comparison_is_surfaced() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, DOMINATING_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [(
                GuardKind::Compare,
                Some("ugt"),
                Some("16"),
                GuardRelation::Dominates
            )]
        );
        assert!(r.lost.is_none(), "{r:?}");
        assert!(r.guards.iter().all(|guard| guard.depth == 0), "{r:?}");

        let r = guards(&session, &local("f", "len"), None).unwrap();
        assert_eq!(
            sites(&r),
            [(
                GuardKind::Compare,
                Some("ugt"),
                Some("16"),
                GuardRelation::OnPath
            )]
        );
        assert!(r.lost.is_none(), "{r:?}");
    }

    /// `16 > len` reads as `len < 16`: the traced value goes on the left.
    #[test]
    fn a_bound_on_the_left_is_read_from_the_traced_value() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, BOUND_ON_LEFT_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [(
                GuardKind::Compare,
                Some("ult"),
                Some("16"),
                GuardRelation::Dominates
            )]
        );
    }

    #[test]
    fn a_saturation_from_the_bound_is_read_from_the_traced_value() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, SATURATE_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [(
                GuardKind::Saturate,
                Some("usub.sat.rev"),
                Some("16"),
                GuardRelation::Defines
            )]
        );
    }

    #[test]
    fn a_comparison_that_does_not_dominate_the_use_is_on_path() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, ON_PATH_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [
                (
                    GuardKind::Compare,
                    Some("ugt"),
                    Some("16"),
                    GuardRelation::OnPath
                ),
                (
                    GuardKind::Compare,
                    Some("ult"),
                    Some("4"),
                    GuardRelation::OnPath
                ),
            ]
        );
    }

    #[test]
    fn a_value_with_no_guard_has_none_and_is_not_lost() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, UNGUARDED_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert!(r.guards.is_empty(), "{r:?}");
        assert!(r.crossings.is_empty(), "{r:?}");
        assert!(r.lost.is_none(), "{r:?}");
    }

    #[test]
    fn a_pointer_round_trip_loses_the_trace_but_keeps_the_guards() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, ROUND_TRIP_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [(
                GuardKind::Clamp,
                Some("umin"),
                Some("16"),
                GuardRelation::Defines
            )]
        );
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(lost.contains("ptrtoint"), "{lost}");
    }

    #[test]
    fn a_parameter_crosses_into_the_anchor_from_each_caller() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, CROSSING_SOURCE, &["-O0", "-g"]);
        let anchor = sink_argument(&session, "callee", 0);
        let r = guards(&session, &anchor, None).unwrap();
        assert_eq!(
            crossings(&r),
            [
                ("caller", CrossingDir::Into, "callee", "parameter 0"),
                ("other", CrossingDir::Into, "callee", "parameter 0"),
            ]
        );
        assert_eq!(
            r.crossings[0].site,
            sites_calling(&session, "caller", "callee")[0]
        );

        let bounded = guards(&session, &anchor, Some("caller")).unwrap();
        assert_eq!(
            crossings(&bounded),
            [("caller", CrossingDir::Into, "callee", "parameter 0")],
            "only callers on the slice from `caller`"
        );
        let error = guards(&session, &anchor, Some("nowhere"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("nowhere"), "{error}");
        let error = guards(&session, &anchor, Some("lone"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not on a path from \"lone\""), "{error}");
    }

    #[test]
    fn a_call_result_crosses_from_its_callee() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, RETURN_SOURCE, &["-O0", "-g"]);
        let get = sites_calling(&session, "f", "get")[0].clone();
        let r = guards(&session, &GuardAnchor::Return { site: get.clone() }, None).unwrap();
        assert_eq!(crossings(&r), [("f", CrossingDir::From, "get", "return")]);
        assert_eq!(r.crossings[0].site, get);

        let passed = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            crossings(&passed),
            [("f", CrossingDir::From, "get", "return")]
        );
    }

    /// The lifetime markers `mem2reg` deletes share the calls' location;
    /// counting them would map the `sink` call onto another.
    #[test]
    fn a_macro_call_resolves_past_its_deleted_lifetime_markers() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(
            &scratch,
            LIFETIME_SOURCE,
            &["-O1", "-Xclang", "-disable-llvm-passes", "-g"],
        );
        assert!(
            session.call_sites().iter().any(|site| matches!(
                &site.target,
                CallTarget::Intrinsic { name } if name.starts_with("llvm.lifetime.")
            )),
            "the fixture must keep its lifetime markers"
        );
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(crossings(&r), [("f", CrossingDir::From, "get", "return")]);
        assert!(r.lost.is_none(), "{r:?}");
    }

    #[test]
    fn a_value_out_of_asm_goto_loses_the_trace() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, ASM_GOTO_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(lost.contains("asm goto"), "{r:?}");
        assert!(r.guards.is_empty() && r.crossings.is_empty(), "{r:?}");
    }

    #[test]
    fn the_result_of_a_void_call_is_refused_naming_the_site() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, VOID_RETURN_SOURCE, &["-O0", "-g"]);
        let site = sites_calling(&session, "f", "act")[0].clone();
        let error = guards(&session, &GuardAnchor::Return { site }, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("returns no value") && error.contains("in f"),
            "{error}"
        );
    }

    #[test]
    fn an_ambiguous_local_is_reported_not_traced() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, LOCAL_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &local("g", "x"), None).unwrap();
        assert!(r.guards.is_empty() && r.crossings.is_empty(), "{r:?}");
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(
            lost.contains("`x`") && lost.contains("more than one value"),
            "{lost}"
        );

        let a = guards(&session, &local("g", "a"), None).unwrap();
        assert!(a.lost.is_none(), "{a:?}");
    }

    #[test]
    fn llvm_diagnostics_on_the_anchor_module_are_reported() {
        let scratch = tempfile::tempdir().unwrap();
        let module = stale_debug_info_module(scratch.path());
        let session = module_session(scratch.path(), &module);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert!(
            r.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("invalid version")),
            "{r:?}"
        );
        assert!(r.lost.is_none(), "a diagnostic is not a lost trace: {r:?}");
    }

    #[test]
    fn a_branch_that_does_not_decide_the_value_is_no_clamp() {
        for (source, flags) in [
            (GOTO_SOURCE, &["-O1", "-g"][..]),
            (GOTO_SOURCE, &["-O0", "-g"]),
            (UNRELATED_CHOICE_SOURCE, &["-O0", "-g"]),
            (UNRELATED_CHOICE_SOURCE, &["-O1", "-g"]),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, source, flags);
            let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
            assert!(
                r.guards.iter().all(|guard| guard.op != GuardKind::Clamp),
                "{flags:?}: {r:?}"
            );
        }
    }

    /// The result is constrained only after the call: those guards consume
    /// it and lie on its path.
    #[test]
    fn a_call_result_surfaces_the_guards_that_consume_it() {
        for optimization in ["-O0", "-O1"] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, RESULT_SOURCE, &[optimization, "-g"]);
            let get = sites_calling(&session, "f", "get")[0].clone();
            let r = guards(&session, &GuardAnchor::Return { site: get }, None).unwrap();
            let mut expected = vec![
                (
                    GuardKind::Compare,
                    Some("ugt"),
                    Some("100"),
                    GuardRelation::OnPath,
                ),
                (
                    GuardKind::Clamp,
                    Some("umin"),
                    Some("16"),
                    GuardRelation::OnPath,
                ),
            ];
            // At -O0 the clamp is a branch on `r < 16`, which compares `r`
            // and is listed too; at -O1 it is `llvm.umin`, with no compare.
            if optimization == "-O0" {
                expected.insert(
                    1,
                    (
                        GuardKind::Compare,
                        Some("ult"),
                        Some("16"),
                        GuardRelation::OnPath,
                    ),
                );
            }
            assert_eq!(sites(&r), expected, "{optimization}: {r:?}");
            assert_eq!(crossings(&r), [("f", CrossingDir::From, "get", "return")]);
            // `r` is compared itself, but only the clamp's result is capped
            // at 16: the clamp is a lead about `r`, never a bound on it.
            let (compare, clamp) = (&r.guards[0], r.guards.last().unwrap());
            if optimization == "-O0" {
                // Its branch joins before `r` is used: no edge says which
                // side holds.
                let surfaced = &r.guards[1];
                assert!(!surfaced.verified, "{r:?}");
                assert_eq!(
                    surfaced.caveat.as_deref(),
                    Some("the bound could not be mapped to the anchored value"),
                    "{r:?}"
                );
            }
            assert!(compare.verified, "{optimization}: {r:?}");
            assert_eq!(compare.affine, Some(Affine::IDENTITY), "{optimization}");
            assert!(!clamp.verified, "{optimization}: {r:?}");
            assert_eq!(clamp.affine, None, "{optimization}: {r:?}");
            assert_eq!(
                clamp.caveat.as_deref(),
                Some("the guard bounds a value computed from the anchored one"),
                "{optimization}: {r:?}"
            );
        }
    }

    /// A clamp computed from the anchored value bounds its own result, but
    /// its comparison bounds the anchored value: on the edge into the call
    /// that passes it, `len < 16` holds.
    #[test]
    fn a_downstream_clamps_comparison_on_the_edge_to_the_use_is_verified() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, DOWNSTREAM_CLAMP_DECIDING_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [
                (
                    GuardKind::Compare,
                    Some("ult"),
                    Some("16"),
                    GuardRelation::Dominates
                ),
                (
                    GuardKind::Clamp,
                    Some("umin"),
                    Some("16"),
                    GuardRelation::OnPath
                ),
            ],
            "{r:?}"
        );
        let (compare, clamp) = (&r.guards[0], &r.guards[1]);
        assert!(compare.verified, "{r:?}");
        assert_eq!(compare.caveat, None, "{r:?}");
        assert_eq!(compare.affine, Some(Affine::IDENTITY), "{r:?}");
        assert!(!clamp.verified, "{r:?}");
        assert_eq!(
            clamp.caveat.as_deref(),
            Some("the guard bounds a value computed from the anchored one"),
            "{r:?}"
        );
    }

    /// The clamp's comparison runs before the use, but decides no edge into
    /// it: which side holds there is not known, so it is a lead.
    #[test]
    fn a_downstream_clamps_comparison_deciding_no_edge_to_the_use_is_a_candidate() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, DOWNSTREAM_CLAMP_JOINED_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [
                (
                    GuardKind::Compare,
                    Some("ult"),
                    Some("16"),
                    GuardRelation::Dominates
                ),
                (
                    GuardKind::Clamp,
                    Some("umin"),
                    Some("16"),
                    GuardRelation::OnPath
                ),
            ],
            "{r:?}"
        );
        let compare = &r.guards[0];
        assert!(!compare.verified, "{r:?}");
        assert_eq!(
            compare.caveat.as_deref(),
            Some("the bound could not be mapped to the anchored value"),
            "{r:?}"
        );
        assert_eq!(compare.affine, Some(Affine::IDENTITY), "{r:?}");
    }

    /// A `select` clamp of the anchored value: its comparison decides no
    /// branch in `selected`, and also a branch into the use in `decided`.
    const DOWNSTREAM_SELECT_IR: &str = r#"
declare void @sink(i32)
declare void @keep(i32)

define void @selected(i32 %n) {
  %c = icmp ult i32 %n, 16
  %m = select i1 %c, i32 %n, i32 16
  call void @sink(i32 %n)
  call void @keep(i32 %m)
  ret void
}

define void @decided(i32 %n) {
  %c = icmp ult i32 %n, 16
  %m = select i1 %c, i32 %n, i32 16
  br i1 %c, label %use, label %out
use:
  call void @sink(i32 %n)
  br label %out
out:
  call void @keep(i32 %m)
  ret void
}
"#;

    /// A `select` clamp's comparison is listed beside the clamp, verified
    /// only when it also decides the branch into the use.
    #[test]
    fn a_downstream_select_clamps_comparison_is_verified_only_on_a_deciding_edge() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            DOWNSTREAM_SELECT_IR,
            &["-Wno-override-module"],
        );
        let no_edge = "the bound could not be mapped to the anchored value";
        let downstream = "the guard bounds a value computed from the anchored one";
        for (function, verified, caveat) in
            [("selected", false, Some(no_edge)), ("decided", true, None)]
        {
            let r = affine_ir_guards(&session, function, "sink");
            assert_eq!(
                sites(&r),
                [
                    (
                        GuardKind::Compare,
                        Some("ult"),
                        Some("16"),
                        GuardRelation::Dominates
                    ),
                    (
                        GuardKind::Clamp,
                        Some("umin"),
                        Some("16"),
                        GuardRelation::OnPath
                    ),
                ],
                "{function}: {r:?}"
            );
            let (compare, clamp) = (&r.guards[0], &r.guards[1]);
            assert_eq!(compare.verified, verified, "{function}: {r:?}");
            assert_eq!(compare.caveat.as_deref(), caveat, "{function}: {r:?}");
            assert_eq!(compare.affine, Some(Affine::IDENTITY), "{function}: {r:?}");
            assert!(!clamp.verified, "{function}: {r:?}");
            assert_eq!(
                clamp.caveat.as_deref(),
                Some(downstream),
                "{function}: {r:?}"
            );
        }
    }

    #[test]
    fn a_value_an_intrinsic_reads_from_memory_loses_the_trace() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            READING_INTRINSIC_IR,
            &["-Wno-override-module"],
        );
        for (n, callee, intrinsic) in [
            (0, "sink", "llvm.read_register."),
            (0, "sinkv", "llvm.masked.load."),
        ] {
            let site = sites_calling(&session, "f", callee)[n].clone();
            let r = guards(&session, &argument(&site, 0), None).unwrap();
            let lost = r.lost.as_deref().unwrap_or_default();
            assert!(lost.contains(intrinsic), "{r:?}");
        }
    }

    /// Its pointer argument is not where the value comes from: following it
    /// would claim a crossing into `h` that carries no such value.
    #[test]
    fn an_intrinsic_outside_the_pure_ones_loses_the_trace() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            TARGET_LOAD_IR,
            &["-Wno-override-module"],
        );
        let r = guards(&session, &sink_argument(&session, "h", 0), None).unwrap();
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(lost.contains("llvm.aarch64.ldxr"), "{r:?}");
        assert!(r.crossings.is_empty(), "no crossing into h: {r:?}");
    }

    /// A guard on a value the anchor is computed from is not a guard on the
    /// anchor: it says how many steps upstream it lies.
    #[test]
    fn a_guard_on_an_operand_says_how_far_upstream_it_is() {
        for (source, op) in [
            (MASKED_OPERAND_SOURCE, GuardKind::Mask),
            (COMPARED_OPERAND_SOURCE, GuardKind::Compare),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, source, &["-O0", "-g"]);
            let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
            let depths: Vec<(GuardKind, u32)> = r
                .guards
                .iter()
                .map(|guard| (guard.op, guard.depth))
                .collect();
            assert_eq!(depths, [(op, 1)], "{r:?}");
        }
    }

    /// The one guard `result` reports.
    fn only_guard(result: &GuardsResult) -> &GuardSite {
        assert_eq!(result.guards.len(), 1, "{result:?}");
        &result.guards[0]
    }

    /// The guards of `function`'s value handed to `callee` first, in an IR
    /// fixture such as [`AFFINE_IR`].
    fn affine_ir_guards(session: &Session, function: &str, callee: &str) -> GuardsResult {
        let site = &sites_calling(session, function, callee)[0];
        guards(session, &argument(site, 0), None).unwrap()
    }

    /// Constant arithmetic between a bounded value and the anchored one is
    /// recorded as the affine relation `anchored = scale * guarded + offset`;
    /// a shift past the width, an `i128` overflow, or two paths that disagree
    /// leave none.
    #[test]
    fn constant_arithmetic_to_the_anchor_is_an_affine_relation() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        let affine = |scale, offset| Some(Affine { scale, offset });
        for (function, callee, expected) in [
            ("offset", "sink", affine(1, 1)),
            ("scaled", "sink", affine(4, 0)),
            ("reversed", "sink", affine(-1, 10)),
            ("composed", "sink", affine(6, -10)),
            ("overshifted", "sink", None),
            ("overflowing", "sink64", None),
            ("divergent", "sink", None),
            ("agreeing", "sink", affine(1, 1)),
            ("partial", "sink", None),
        ] {
            let r = affine_ir_guards(&session, function, callee);
            assert_eq!(only_guard(&r).affine, expected, "{function}: {r:?}");
        }
    }

    /// A widening whose signedness matches the guard's keeps the value; one
    /// that does not, a truncation, or a widening mixed with arithmetic does
    /// not. A comparison in the chain bounds its operand, which the anchored
    /// boolean is no affine function of.
    #[test]
    fn only_a_matching_widening_alone_is_the_identity() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        for (function, callee, expected) in [
            ("widened", "sink64", Some(Affine::IDENTITY)),
            ("signed_widened", "sink64", Some(Affine::IDENTITY)),
            ("mismatched", "sink64", None),
            ("widened_then_added", "sink64", None),
            ("added_then_widened", "sink64", None),
            ("truncated", "sink", None),
            ("flag", "sink", None),
        ] {
            let r = affine_ir_guards(&session, function, callee);
            assert_eq!(only_guard(&r).affine, expected, "{function}: {r:?}");
        }
    }

    /// A guard on a value the anchored one is no constant affine function
    /// of is a lead, not a bound on it: unverified, saying why. One that is
    /// stays verified.
    #[test]
    fn a_guard_with_no_affine_relation_is_unverified() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        let r = affine_ir_guards(&session, "summed", "sink");
        let site = only_guard(&r);
        assert_eq!(site.affine, None, "{r:?}");
        assert!(!site.verified, "{r:?}");
        assert_eq!(
            site.caveat.as_deref(),
            Some("the relation to the anchored value is not affine")
        );

        for function in ["offset", "agreeing"] {
            let r = affine_ir_guards(&session, function, "sink");
            let site = only_guard(&r);
            assert!(site.verified && site.caveat.is_none(), "{function}: {r:?}");
        }
    }

    /// A bound carried through its affine relation must stay within the
    /// anchored value's range at its width and signedness, from either end
    /// of the interval the guard admits; one that wraps keeps its relation
    /// but is unverified.
    #[test]
    fn a_bound_that_wraps_when_carried_to_the_anchor_is_unverified() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        let wraps = Some("the bound does not carry to the anchored value without wraparound");
        let affine = |scale, offset| Some(Affine { scale, offset });
        for (function, callee, relation, caveat) in [
            // [0, 254] + 2 reaches 256 at 8 bits.
            ("wrapping", "sink8", affine(1, 2), wraps),
            ("carrying", "sink8", affine(1, 2), None),
            // `-16` at 8 bits, read unsigned, is 240: [0, 239] + 10 carries,
            // and + 20 reaches 259.
            ("unsigned_negative", "sink8", affine(1, 10), None),
            ("unsigned_negative_wrapping", "sink8", affine(1, 20), wraps),
            // The anchor is reached on the false edge of `x ugt 16`, where
            // `x ule 16` holds: [0, 16] + 1 carries.
            ("above", "sink", affine(1, 1), None),
            // ... and on the false edge of `x ult 16`, where `x uge 16`
            // admits the maximum, which + 1 wraps.
            ("false_edge", "sink", affine(1, 1), wraps),
            // 10 - [0, 15] goes below zero, unsigned; 10 - [0, 10] does not.
            ("reversed", "sink", affine(-1, 10), wraps),
            ("reversed_within", "sink", affine(-1, 10), None),
            ("signed_offset", "sink8", affine(1, 1), None),
            ("signed_wrapping", "sink8", affine(1, 100), wraps),
            // The clamp's result, [0, 16], is what the anchor adds 1 to.
            ("clamped_then_added", "sink", affine(1, 1), None),
        ] {
            let r = affine_ir_guards(&session, function, callee);
            let site = only_guard(&r);
            assert_eq!(site.affine, relation, "{function}: {r:?}");
            assert_eq!(site.verified, caveat.is_none(), "{function}: {r:?}");
            assert_eq!(site.caveat.as_deref(), caveat, "{function}: {r:?}");
        }
    }

    /// A bound only carries from an edge that decides it: one into a block
    /// that dominates the anchor and is entered from nothing else. Without
    /// one, without a constant bound, or past a comparison no value passes,
    /// such as `x ugt UMAX`, nothing is known to carry.
    #[test]
    fn a_bound_with_no_deciding_edge_or_constant_cannot_be_carried() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        for function in [
            "shared_successor",
            "unbounded",
            "clamped_to_value",
            "never_above",
        ] {
            let r = affine_ir_guards(&session, function, "sink");
            let site = only_guard(&r);
            assert_eq!(
                site.affine,
                Some(Affine {
                    scale: 1,
                    offset: 1
                }),
                "{function}: {r:?}"
            );
            assert!(!site.verified, "{function}: {r:?}");
            assert_eq!(
                site.caveat.as_deref(),
                Some("the bound could not be mapped to the anchored value"),
                "{function}: {r:?}"
            );
        }
    }

    /// A comparison on the anchored value itself bounds it however it reads
    /// its integers; across a widening, an equality's negative bound is
    /// another number once zero-extended.
    #[test]
    fn a_guard_on_the_anchor_itself_is_verified_whatever_its_signedness() {
        let scratch = tempfile::tempdir().unwrap();
        let session =
            compiled_session(scratch.path(), "t.ll", AFFINE_IR, &["-Wno-override-module"]);
        for (function, callee, expected) in [
            ("unsigned_itself", "sink", Some(Affine::IDENTITY)),
            ("signed_itself", "sink", Some(Affine::IDENTITY)),
            ("equal_itself", "sink", Some(Affine::IDENTITY)),
            ("equal_widened", "sink64", Some(Affine::IDENTITY)),
            ("negative_equal_widened", "sink64", None),
            (
                "negative_equal_sign_extended",
                "sink64",
                Some(Affine::IDENTITY),
            ),
        ] {
            let r = affine_ir_guards(&session, function, callee);
            let site = only_guard(&r);
            assert_eq!(site.affine, expected, "{function}: {r:?}");
            assert_eq!(site.verified, expected.is_some(), "{function}: {r:?}");
        }
    }

    /// Compiled from C: `(len & 0xff) + 1000` and `len * 4`.
    #[test]
    fn a_guard_upstream_of_constant_arithmetic_carries_its_relation() {
        // The mask admits [0, 255], so the anchor lies in [1000, 1255]; past
        // `if (len > 16) return;`, `len` is in [0, 16], so `len * 4` in
        // [0, 64].
        for (source, scale, offset, verified) in [
            (MASKED_OPERAND_SOURCE, 1, 1000, true),
            (COMPARED_OPERAND_SOURCE, 4, 0, true),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, source, &["-O0", "-g"]);
            let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
            let site = only_guard(&r);
            assert_eq!(
                site.affine,
                Some(Affine { scale, offset }),
                "{source}: {r:?}"
            );
            assert_eq!(site.verified, verified, "{source}: {r:?}");
        }
    }

    /// A truncation does not keep the value, so a guard before one is a
    /// step upstream; a widening does, so a guard before one is on the
    /// anchor.
    #[test]
    fn a_guard_before_a_truncation_is_a_step_upstream() {
        for (source, op, depth) in [
            (TRUNCATED_SOURCE, GuardKind::Compare, 1),
            (NARROWED_SOURCE, GuardKind::Compare, 1),
            (WIDENED_SOURCE, GuardKind::Compare, 0),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let session = session(&scratch, source, &["-O0", "-g"]);
            let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
            let depths: Vec<(GuardKind, u32)> = r
                .guards
                .iter()
                .map(|guard| (guard.op, guard.depth))
                .collect();
            assert_eq!(depths, [(op, depth)], "{source}: {r:?}");
        }
    }

    /// The IR prints `0xfffffff0` as `-16` and `0x80000000` as
    /// `-2147483648`; an unsigned comparison's or a mask's bound is the
    /// unsigned value.
    #[test]
    fn an_unsigned_bound_is_the_unsigned_value() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, UNSIGNED_BOUNDS_SOURCE, &["-O0", "-g"]);
        let r = guards(&session, &sink_argument(&session, "f", 0), None).unwrap();
        assert_eq!(
            sites(&r),
            [
                (
                    GuardKind::Compare,
                    Some("ult"),
                    Some("2147483648"),
                    GuardRelation::Dominates
                ),
                (
                    GuardKind::Mask,
                    None,
                    Some("4294967280"),
                    GuardRelation::Defines
                ),
            ],
            "{r:?}"
        );
    }

    #[test]
    fn a_bound_reads_unsigned_only_for_an_unsigned_operation_of_known_width() {
        let read = |op, predicate, bound, bits| unsigned_bound(op, predicate, bound, bits);
        assert_eq!(read(GuardKind::Mask, None, "-16", Some(32)), "4294967280");
        assert_eq!(read(GuardKind::Clamp, Some("umin"), "-1", Some(8)), "255");
        assert_eq!(
            read(GuardKind::Saturate, Some("usub.sat.rev"), "-1", Some(16)),
            "65535"
        );
        assert_eq!(
            read(GuardKind::Compare, Some("slt"), "-16", Some(32)),
            "-16"
        );
        assert_eq!(read(GuardKind::Compare, Some("eq"), "-16", Some(32)), "-16");
        assert_eq!(
            read(GuardKind::Clamp, Some("umin"), "-1", None),
            "-1",
            "an unknown width leaves the IR's spelling"
        );
        assert_eq!(read(GuardKind::Mask, None, "true", Some(1)), "true");
    }

    /// A guard site `x predicate bound` at depth 1, `affine` from the
    /// anchored value.
    fn carried_site(predicate: &str, bound: &str, affine: Affine) -> GuardSite {
        GuardSite {
            op: GuardKind::Compare,
            predicate: Some(predicate.to_string()),
            bound: Some(bound.to_string()),
            location: None,
            relation: GuardRelation::Dominates,
            depth: 1,
            verified: true,
            caveat: None,
            affine: Some(affine),
        }
    }

    /// The IR prints `240` at 8 bits as `-16`: an unsigned comparison reads
    /// it as 240 however it is spelled, so `x ult -16` admits [0, 239],
    /// which `x + 10` keeps within 8 bits. A signed one reads it as -16.
    #[test]
    fn a_bound_is_read_in_its_guards_own_signedness() {
        let plus_ten = Affine {
            scale: 1,
            offset: 10,
        };
        for (predicate, bound) in [("ult", "-16"), ("ult", "240"), ("slt", "-16")] {
            let site = carried_site(predicate, bound, plus_ten);
            assert_eq!(
                uncarried(&site, Some(8), Some(true)),
                None,
                "{predicate} {bound}"
            );
        }
        let site = carried_site(
            "ult",
            "-16",
            Affine {
                scale: 1,
                offset: 20,
            },
        );
        assert_eq!(
            uncarried(&site, Some(8), Some(true)),
            Some(Unverified::Wraps),
            "[0, 239] + 20 reaches 259"
        );
    }

    /// `ult -1` at 127 bits is `ult 2^127 - 1`, the widest unsigned value
    /// an i128 holds, read without forming 2^127. At 128 bits the unsigned
    /// range does not fit in an i128, so the bound stays unread.
    #[test]
    fn a_negative_bound_reads_unsigned_at_127_bits() {
        assert_eq!(read_bound("-1", 127, false), Some(i128::MAX));
        assert_eq!(read_bound("-1", 128, false), None);
    }

    /// A bound the guard's integer cannot hold, or one with no width or no
    /// number to read, is not known to wrap: it maps to nothing.
    #[test]
    fn a_bound_its_guard_cannot_hold_is_unmappable() {
        let plus_one = Affine {
            scale: 1,
            offset: 1,
        };
        for (predicate, bound, bits) in [
            ("ult", "300", Some(8)),
            ("ult", "-200", Some(8)),
            ("slt", "200", Some(8)),
            ("ult", "sixteen", Some(8)),
            ("ult", "16", None),
        ] {
            let site = carried_site(predicate, bound, plus_one);
            assert_eq!(
                uncarried(&site, bits, Some(true)),
                Some(Unverified::Unmappable),
                "{predicate} {bound} at {bits:?}"
            );
        }
    }

    /// At 8 bits, `-128` is the lowest signed spelling, so unsigned it is
    /// 128; `-129` is no 8-bit integer. A guard that passes no value --
    /// `x ult 0`, `x urem 0` -- has no interval to carry.
    #[test]
    fn a_bound_at_the_edge_of_its_width_is_read_or_unmappable() {
        let plus_one = Affine {
            scale: 1,
            offset: 1,
        };
        assert_eq!(read_bound("-128", 8, false), Some(128));
        assert_eq!(read_bound("-129", 8, false), None);
        assert_eq!(
            uncarried(&carried_site("ult", "-128", plus_one), Some(8), Some(true)),
            None,
            "[0, 127] + 1 stays within 8 bits"
        );
        let modulo = GuardSite {
            op: GuardKind::Modulo,
            ..carried_site("urem", "0", plus_one)
        };
        for (site, what) in [
            (carried_site("ult", "-129", plus_one), "ult -129"),
            (carried_site("ult", "0", plus_one), "ult 0"),
            (modulo, "urem 0"),
        ] {
            assert_eq!(
                uncarried(&site, Some(8), Some(true)),
                Some(Unverified::Unmappable),
                "{what}"
            );
        }
    }

    #[test]
    fn a_from_naming_several_functions_is_rejected_naming_them() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(scratch.path(), "t.cc", TWO_PARSERS_SOURCE, &["-O0", "-g"]);
        let anchor = sink_argument(&session, "_Z3usej", 0);
        let error = guards(&session, &anchor, Some("parse"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("names 2 defined functions")
                && error.contains("_ZN1a5parseEj")
                && error.contains("_ZN1b5parseEj"),
            "{error}"
        );
        let one = guards(&session, &anchor, Some("_ZN1a5parseEj")).unwrap();
        assert_eq!(
            crossings(&one),
            [("_ZN1a5parseEj", CrossingDir::Into, "_Z3usej", "parameter 0")]
        );
    }

    /// The linked module is promoted before anything reaches `out`, so a
    /// promotion that fails leaves no file behind.
    #[test]
    fn a_failed_promotion_leaves_no_emitted_module() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, CROSSING_SOURCE, &["-O0", "-g"]);
        let bindir = scratch.path().join("bin");
        std::fs::create_dir(&bindir).unwrap();
        for tool in ["llvm-extract", "llvm-nm"] {
            std::os::unix::fs::symlink(rllvm_testkit::llvm_bin(tool), bindir.join(tool)).unwrap();
        }
        let real_opt = rllvm_testkit::llvm_bin("opt");
        let major = rllvm_core::diagnostics::tool_llvm_major_version(&real_opt).unwrap();
        let opt = bindir.join("opt");
        rllvm_testkit::write_executable(
            &opt,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'LLVM version {major}.0.0'; exit 0; }}\nexit 1\n"
            ),
        );

        let out = scratch.path().join("guards.bc");
        let result = emit_guards_module(
            &session,
            session.catalog_path().unwrap(),
            &sink_argument(&session, "callee", 0),
            None,
            &rllvm_testkit::llvm_bin("llvm-link"),
            &LlvmBindir::configured(&bindir),
            &out,
        );
        assert!(result.is_err(), "{result:?}");
        assert!(!out.exists(), "a failed promotion wrote {}", out.display());
    }

    #[test]
    fn the_common_dominator_of_two_blocks_is_the_nearest_above_both() {
        // 0 -> {1, 2} -> 3; 4 is unreachable.
        let d = dominators(&cfg(&[&[1, 2], &[3], &[3], &[], &[3]]));
        assert_eq!(d.common_dominator(1, 2), Some(0));
        assert_eq!(d.common_dominator(1, 3), Some(0));
        assert_eq!(d.common_dominator(0, 3), Some(0));
        assert_eq!(d.common_dominator(3, 3), Some(3));
        assert_eq!(d.common_dominator(4, 3), None);
    }

    /// A diamond, 0 -> {1, 2} -> 3, holding instructions 0 and 1 in block
    /// 0, 2 and 3 in block 1, 4 in block 2 and 5 in block 3.
    fn diamond_of_instructions() -> FunctionIr {
        let mut body = cfg(&[&[1, 2], &[3], &[3], &[]]);
        for (id, block) in [0, 0, 1, 1, 2, 3].into_iter().enumerate() {
            body.blocks[block].instrs.push(id as ValueId);
            body.values.push(ValueNode {
                block,
                opcode: OpcodeIr::Other,
                operands: Vec::new(),
                incoming: Vec::new(),
                call: None,
                guard: None,
                location: None,
                volatile: false,
                ty: None,
                element: None,
            });
        }
        body
    }

    /// Uses on both arms share the branch above them: a guard running only
    /// before one of them is not before the use-site.
    #[test]
    fn the_point_uses_share_runs_before_each_of_them() {
        let body = diamond_of_instructions();
        let d = dominators(&body);
        assert_eq!(
            shared_point(&body, &d, &[3, 4]),
            1,
            "the branch above both arms"
        );
        assert_eq!(
            shared_point(&body, &d, &[4, 5]),
            1,
            "an arm is not above the join"
        );
        assert_eq!(
            shared_point(&body, &d, &[3, 2]),
            2,
            "the first in one block"
        );
        assert_eq!(
            shared_point(&body, &d, &[5, 0]),
            0,
            "the use above the other"
        );
    }

    #[test]
    fn dominators_on_a_diamond() {
        let d = dominators(&cfg(&[&[1, 2], &[3], &[3], &[]]));
        for b in 0..4 {
            assert!(d.dominates(0, b));
            assert!(d.dominates(b, b));
        }
        assert!(!d.dominates(1, 3));
        assert!(!d.dominates(2, 3));
        assert!(!d.dominates(1, 2));
        assert!(!d.dominates(3, 0));
    }

    #[test]
    fn dominators_through_a_loop_back_edge() {
        // 0 -> 1 -> 2 -> {1, 3}
        let d = dominators(&cfg(&[&[1], &[2], &[1, 3], &[]]));
        assert!(d.dominates(1, 2));
        assert!(d.dominates(1, 3));
        assert!(d.dominates(2, 3));
        assert!(!d.dominates(2, 1));
        assert!(!d.dominates(3, 1));
    }

    #[test]
    fn dominators_ignore_unreachable_blocks() {
        // Block 2 and 3 are unreachable; 3 branches into the reachable 1.
        let d = dominators(&cfg(&[&[1], &[], &[1], &[1, 2]]));
        assert!(d.dominates(0, 1));
        assert!(!d.dominates(2, 1));
        assert!(!d.dominates(3, 1));
        assert!(!d.dominates(0, 2));
        assert!(!d.dominates(2, 2));
        assert!(!d.dominates(0, 99));
    }

    fn parts(
        at: Option<&str>,
        operand: Option<usize>,
        ret: bool,
        function: Option<&str>,
        local: Option<&str>,
    ) -> AnchorParts {
        AnchorParts {
            at: at.map(str::to_string),
            operand,
            ret,
            function: function.map(str::to_string),
            local: local.map(str::to_string),
        }
    }

    #[test]
    fn a_line_with_one_call_resolves_to_its_site() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.c",
            "extern void sink(unsigned);\nvoid f(unsigned n) {\n  sink(n);\n}\n",
            &["-O0", "-g"],
        );
        let site = session
            .call_sites()
            .iter()
            .find(|site| site.id.function.symbol == "f")
            .unwrap()
            .id
            .clone();
        let at = format!("{}:3", scratch.path().join("t.c").display());
        let anchor = anchor_from_parts(
            &session,
            &parts(Some(&at), Some(0), false, None, None),
            AnchorSurface::Cli,
        );
        match anchor.unwrap() {
            GuardAnchor::Argument {
                site: found,
                operand,
            } => {
                assert_eq!((found, operand), (site.clone(), 0));
            }
            other => panic!("{other:?}"),
        }
        let json = serde_json::to_string(&site).unwrap();
        let spelled = anchor_from_parts(
            &session,
            &parts(Some(&json), None, true, None, None),
            AnchorSurface::Cli,
        )
        .unwrap();
        assert!(matches!(spelled, GuardAnchor::Return { site: found } if found == site));
        let blank = format!("{}:1", scratch.path().join("t.c").display());
        let missing = anchor_from_parts(
            &session,
            &parts(Some(&blank), None, true, None, None),
            AnchorSurface::Cli,
        );
        let message = missing.unwrap_err();
        assert!(message.contains("no call site at"));
        assert!(message.contains("inlined"));
        assert!(message.contains("--local"));
    }

    #[test]
    fn a_line_with_several_calls_is_ambiguous_and_names_each() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.c",
            "extern unsigned a(unsigned);\nextern void b(unsigned);\nvoid f(unsigned n) {\n  b(a(n));\n}\n",
            &["-O0", "-g"],
        );
        let at = format!("{}:4", scratch.path().join("t.c").display());
        let error = anchor_from_parts(
            &session,
            &parts(Some(&at), Some(0), false, None, None),
            AnchorSurface::Cli,
        )
        .unwrap_err();
        assert!(error.contains("holds 2 call sites"), "{error}");
        assert!(
            error.contains("a in f") && error.contains("b in f"),
            "{error}"
        );
        // Each candidate carries the id to paste back.
        assert_eq!(error.matches("\"block_index\"").count(), 2, "{error}");
        let json = error
            .split("=> ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(
            anchor_from_parts(
                &session,
                &parts(Some(json), None, true, None, None),
                AnchorSurface::Cli
            )
            .is_ok()
        );
    }

    #[test]
    fn a_call_site_id_the_catalog_lacks_is_rejected() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(scratch.path(), "t.c", "void a(void) {}\n", &["-O0"]);
        let ghost =
            r#"{"function":{"module_id":"x","symbol":"y"},"block_index":0,"instruction_index":0}"#;
        let error = anchor_from_parts(
            &session,
            &parts(Some(ghost), None, true, None, None),
            AnchorSurface::Cli,
        )
        .unwrap_err();
        assert!(error.contains("no such call site"), "{error}");
    }

    #[test]
    fn a_combination_of_parts_that_names_no_single_value_is_rejected() {
        let ok = |at, operand, ret, function, local| {
            check_anchor_parts(
                &parts(at, operand, ret, function, local),
                AnchorSurface::Cli,
            )
        };
        assert!(ok(Some("t.c:4"), Some(0), false, None, None).is_ok());
        assert!(ok(Some("t.c:4"), None, true, None, None).is_ok());
        assert!(ok(None, None, false, Some("f"), Some("n")).is_ok());
        assert!(ok(Some("t.c:4"), None, false, Some("f"), Some("n")).is_ok());
        for (parts, fragment) in [
            (
                ok(Some("t.c:4"), None, true, Some("f"), Some("n")),
                "give one",
            ),
            (
                ok(Some("t.c:4"), None, false, None, Some("n")),
                "needs --function",
            ),
            (ok(None, None, false, None, None), "name a value"),
            (ok(None, Some(0), false, None, None), "need --at"),
            (ok(None, None, true, None, None), "need --at"),
            (
                ok(Some("t.c:4"), None, false, None, None),
                "needs --operand",
            ),
            (ok(Some("t.c:4"), Some(0), true, None, None), "give one"),
            (ok(None, None, false, None, Some("n")), "needs --function"),
            (ok(None, None, false, Some("f"), None), "needs --local"),
            (
                ok(Some("t.c:4"), Some(0), false, Some("f"), Some("n")),
                "give one",
            ),
        ] {
            let error = parts.unwrap_err();
            assert!(error.contains(fragment), "{error}");
        }
    }

    /// Field addresses of `%s` and the loads of one: by two GEPs alike, by a
    /// GEP into another field, by a GEP alike but through another type, and
    /// a volatile, an atomic and a wider load.
    const ADDRESSES_IR: &str = r#"
%struct.S = type { i32, i32 }
declare void @sink(i32)
declare void @sink64(i64)

define void @f(ptr %s, i32 %n) {
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %n, ptr %a, align 4
  %b = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  %c = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  %d = getelementptr inbounds { i64, i64 }, ptr %s, i32 0, i32 1
  %v = load i32, ptr %b, align 4
  %w = load volatile i32, ptr %b, align 4
  %x = load atomic i32, ptr %b seq_cst, align 4
  %y = load i64, ptr %b, align 8
  call void @sink(i32 %v)
  call void @sink(i32 %w)
  call void @sink(i32 %x)
  call void @sink64(i64 %y)
  ret void
}
"#;

    /// The trace of `function` in `ir`, read from `session`'s module.
    fn trace_of<'a>(session: &'a Session, ir: &'a ModuleIr, function: &'a FunctionId) -> Trace<'a> {
        let body = defined(ir, &function.symbol);
        Trace {
            session,
            body,
            function,
            scope: None,
            doms: dominators(body),
        }
    }

    /// The ids of `body`'s instructions, in order.
    fn instructions(body: &FunctionIr) -> Vec<ValueId> {
        body.blocks
            .iter()
            .flat_map(|block| block.instrs.clone())
            .collect()
    }

    /// Two GEPs alike are one address; one into another field, or alike but
    /// through another type, is not. A load records its type and whether it
    /// is volatile or atomic; a store, the type of the value it writes.
    #[test]
    fn a_field_address_matches_only_the_same_field_of_the_same_type() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            ADDRESSES_IR,
            &["-Wno-override-module"],
        );
        let function = FunctionId {
            module_id: "m".into(),
            symbol: "f".into(),
        };
        let bytes = promote_anchor_module(&session, &function).unwrap().bytes;
        let ir = read_module_ir(&bytes).unwrap();
        let trace = trace_of(&session, &ir, &function);
        let ids = instructions(trace.body);
        let [a, store, b, c, d, v, w, x, y] = ids[..9] else {
            panic!("{ids:?}");
        };
        let value = Operand::Value;
        assert!(trace.same_address(&value(a), &value(a)));
        assert!(trace.same_address(&value(b), &value(a)));
        assert!(!trace.same_address(&value(c), &value(a)));
        assert!(!trace.same_address(&value(d), &value(a)));
        assert!(!trace.same_address(&Operand::Other, &Operand::Other));

        let int = |bits| {
            Some(TypeTag {
                kind: ScalarKind::Int,
                bits,
            })
        };
        let node = |id| trace.node(id);
        assert_eq!(node(store).opcode, OpcodeIr::Store);
        assert_eq!((node(store).ty, node(store).volatile), (int(32), false));
        assert_eq!((node(v).ty, node(v).volatile), (int(32), false));
        assert_eq!((node(w).ty, node(w).volatile), (int(32), true));
        assert_eq!((node(x).ty, node(x).volatile), (int(32), true));
        assert_eq!((node(y).ty, node(y).volatile), (int(64), false));
        assert_eq!((node(a).ty, node(a).volatile), (None, false));
    }

    /// A clamp stored to `s->len`, reloaded and passed to `sink`: with
    /// nothing or only non-writing operations between, over an earlier
    /// store in its block or in another, through a volatile, an atomic or a
    /// wider load, after a volatile store, and with a literal stored
    /// instead.
    const FIELD_IR: &str = r#"
%struct.S = type { i32, i32 }
declare void @sink(i32)
declare void @sink64(i64)
declare i32 @llvm.umin.i32(i32, i32)

define void @proven(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %b = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  %v = load i32, ptr %b, align 4
  call void @sink(i32 %v)
  ret void
}

define void @allowed(ptr %s, i32 %n, i32 %k) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %t = add i32 %k, 1
  %q = icmp ult i32 %t, 3
  %r = select i1 %q, i32 %t, i32 0
  %w = zext i32 %r to i64
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  %p = load i32, ptr %o, align 4
  %b = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  %v = load i32, ptr %b, align 4
  call void @sink(i32 %v)
  call void @sink64(i64 %w)
  call void @sink(i32 %p)
  ret void
}

define void @volatile_load(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %v = load volatile i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @atomic_load(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %v = load atomic i32, ptr %a acquire, align 4
  call void @sink(i32 %v)
  ret void
}

define void @volatile_store(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store volatile i32 %m, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @wider(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %v = load i64, ptr %a, align 8
  call void @sink64(i64 %v)
  ret void
}

define void @overwritten(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %n, ptr %a, align 4
  store i32 %m, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @overwritten_across(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %n, ptr %a, align 4
  br label %next
next:
  store i32 %m, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @literal(ptr %s) {
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 7, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}
"#;

    /// A field reloaded right after its nearest store, with only
    /// non-writing operations between, is the stored value, whatever was
    /// stored to it earlier: the clamp on it is a verified guard on the
    /// anchor. A volatile or atomic access, or a load of another type, is
    /// never forwarded; a stored literal ends the trace with no guard.
    #[test]
    fn a_clamp_stored_to_a_field_and_reloaded_is_a_verified_guard() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(scratch.path(), "t.ll", FIELD_IR, &["-Wno-override-module"]);
        for function in ["proven", "allowed", "overwritten", "overwritten_across"] {
            let r = affine_ir_guards(&session, function, "sink");
            let site = only_guard(&r);
            assert_eq!(
                (site.op, site.predicate.as_deref(), site.bound.as_deref()),
                (GuardKind::Clamp, Some("umin"), Some("16")),
                "{function}: {r:?}"
            );
            assert!(site.verified, "{function}: {r:?}");
            assert_eq!(site.caveat, None, "{function}: {r:?}");
            assert_eq!(site.affine, Some(Affine::IDENTITY), "{function}: {r:?}");
            assert_eq!(site.depth, 0, "{function}: {r:?}");
            assert!(r.lost.is_none(), "{function}: {r:?}");
        }
        for (function, callee) in [
            ("volatile_load", "sink"),
            ("atomic_load", "sink"),
            ("volatile_store", "sink"),
            ("wider", "sink64"),
        ] {
            let r = affine_ir_guards(&session, function, callee);
            assert!(r.guards.is_empty(), "{function}: {r:?}");
            let lost = r.lost.as_deref().unwrap_or_default();
            assert!(lost.contains("loaded from memory"), "{function}: {r:?}");
        }
        let r = affine_ir_guards(&session, "literal", "sink");
        assert!(r.guards.is_empty(), "{r:?}");
        assert!(r.lost.is_none(), "{r:?}");
    }

    /// The clamp of [`FIELD_IR`] stored to `s->len` and reloaded, with
    /// something that may change the field between -- a call, another
    /// store, or a store to the same field that is volatile or of another
    /// type -- even over an earlier store, or the stores in earlier blocks;
    /// and a load with no store to its field at all.
    const CLOBBERED_IR: &str = r#"
%struct.S = type { i32, i32 }
declare void @sink(i32)
declare void @g(ptr)
declare i32 @llvm.umin.i32(i32, i32)

define void @called(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  call void @g(ptr %s)
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @stored(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  store i32 0, ptr %o, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @atomic(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  %old = atomicrmw add ptr %o, i32 1 seq_cst, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @exchanged(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  %pair = cmpxchg ptr %o, i32 0, i32 1 seq_cst seq_cst, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @volatile_between(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  %x = load volatile i32, ptr %o, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @twice(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  br label %mid
mid:
  store i32 %m, ptr %a, align 4
  br label %next
next:
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @twice_differing(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  br label %mid
mid:
  store i32 7, ptr %a, align 4
  br label %next
next:
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @overwritten_called(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %n, ptr %a, align 4
  br label %next
next:
  store i32 %m, ptr %a, align 4
  call void @g(ptr %s)
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @volatile_nearest(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  store volatile i32 %n, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @atomic_nearest(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  store atomic i32 %n, ptr %a seq_cst, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @narrower_nearest(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %h = trunc i32 %n to i16
  store i16 %h, ptr %a, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @retyped_nearest(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  %b = getelementptr inbounds { i32, i32 }, ptr %s, i32 0, i32 1
  store i32 %n, ptr %b, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @other_block(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  br label %next
next:
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @several_then_called(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  br label %mid
mid:
  store i32 %m, ptr %a, align 4
  br label %next
next:
  call void @g(ptr %s)
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @called_and_stored(ptr %s, i32 %n) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  call void @g(ptr %s)
  %o = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 0
  store i32 0, ptr %o, align 4
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @either(ptr %s, i32 %n, i1 %c) {
  %m = call i32 @llvm.umin.i32(i32 %n, i32 16)
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %m, ptr %a, align 4
  call void @g(ptr %s)
  %v = load i32, ptr %a, align 4
  %x = select i1 %c, i32 %m, i32 %v
  call void @sink(i32 %x)
  ret void
}

define void @unclamped(ptr %s, i32 %n) {
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  store i32 %n, ptr %a, align 4
  call void @g(ptr %s)
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}

define void @unclamped_caller(ptr %s) {
  call void @unclamped(ptr %s, i32 5)
  ret void
}

define void @unstored(ptr %s) {
  %a = getelementptr inbounds %struct.S, ptr %s, i32 0, i32 1
  %v = load i32, ptr %a, align 4
  call void @sink(i32 %v)
  ret void
}
"#;

    /// A store the load may not read back -- something between them may
    /// change the field, or it is in another block, or more than one does
    /// and none in the load's -- still leads to the clamp, as a candidate
    /// saying why. A field nothing stores to stays a lost trace.
    #[test]
    fn a_clamp_stored_to_a_field_that_may_change_is_a_candidate() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            CLOBBERED_IR,
            &["-Wno-override-module"],
        );
        let call = "a call between the store and the load may change the field";
        let store = "another store between the store and the load may change the field";
        let operation = "an operation between the store and the load may change the field";
        let several = "more than one store to this field reaches the load";
        let other_block = "the store and the load are in different blocks";
        let not_affine = "the relation to the anchored value is not affine";
        let call_and_store = format!("{call}; {store}");
        let call_and_several = format!("{call}; {several}");
        // Each fixture's memory cause, then the guard's whole caveat.
        for (function, cause, caveat) in [
            ("called", call, call.to_string()),
            ("stored", store, store.to_string()),
            ("atomic", operation, operation.to_string()),
            ("exchanged", operation, operation.to_string()),
            ("volatile_between", operation, operation.to_string()),
            ("twice", several, several.to_string()),
            (
                "twice_differing",
                several,
                format!("{several}; {not_affine}"),
            ),
            ("other_block", other_block, other_block.to_string()),
            // The nearest store is the one to read back; anything else that
            // may write the field first is never skipped to reach a store.
            ("overwritten_called", call, call.to_string()),
            ("volatile_nearest", store, store.to_string()),
            ("atomic_nearest", store, store.to_string()),
            ("narrower_nearest", store, store.to_string()),
            ("retyped_nearest", store, store.to_string()),
            ("called_and_stored", &call_and_store, call_and_store.clone()),
            // A call in the load's block may change the field whichever
            // of the earlier stores reaches it.
            (
                "several_then_called",
                &call_and_several,
                call_and_several.clone(),
            ),
            // Reached both directly and through the load, the clamp still
            // carries the load's cause.
            ("either", call, call.to_string()),
        ] {
            let r = affine_ir_guards(&session, function, "sink");
            let site = only_guard(&r);
            assert_eq!(
                (site.op, site.predicate.as_deref(), site.bound.as_deref()),
                (GuardKind::Clamp, Some("umin"), Some("16")),
                "{function}: {r:?}"
            );
            assert!(!site.verified, "{function}: {r:?}");
            assert_eq!(site.caveat.as_deref(), Some(caveat.as_str()), "{function}");
            let reason = format!(
                "the value is loaded from memory, and the field may hold something other than \
                 what the store wrote ({cause})"
            );
            assert_eq!(
                r.lost.as_deref(),
                Some(reason.as_str()),
                "{function}: {r:?}"
            );
        }
        let r = affine_ir_guards(&session, "unstored", "sink");
        assert!(r.guards.is_empty(), "{r:?}");
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(lost.contains("loaded from memory"), "{r:?}");
    }

    /// With no guard to report, an unproven load still says the trace is
    /// incomplete: the crossing into the stored parameter is not all the
    /// field may hold.
    #[test]
    fn an_unguarded_field_that_may_change_is_a_lost_trace() {
        let scratch = tempfile::tempdir().unwrap();
        let session = compiled_session(
            scratch.path(),
            "t.ll",
            CLOBBERED_IR,
            &["-Wno-override-module"],
        );
        let r = affine_ir_guards(&session, "unclamped", "sink");
        assert!(r.guards.is_empty(), "{r:?}");
        assert_eq!(
            crossings(&r),
            [(
                "unclamped_caller",
                CrossingDir::Into,
                "unclamped",
                "parameter 1"
            )],
            "{r:?}"
        );
        let lost = r.lost.as_deref().unwrap_or_default();
        assert!(
            lost.contains("the field may hold something other than what the store wrote"),
            "{r:?}"
        );
    }

    /// A use is a line, not a call: an id names one call, and a use-site
    /// that does not parse is refused before any catalog is read.
    #[test]
    fn a_use_site_for_a_local_is_a_file_line_never_a_call_site_id() {
        let check = |at: &str, surface| {
            check_anchor_parts(&parts(Some(at), None, false, Some("f"), Some("n")), surface)
        };
        let id =
            r#"{"function":{"module_id":"m","symbol":"f"},"block_index":0,"instruction_index":0}"#;
        let error = check(id, AnchorSurface::Cli).unwrap_err();
        assert!(
            error.contains("--at") && error.contains("file:line") && error.contains("call site id"),
            "{error}"
        );
        let error = check(id, AnchorSurface::Tool).unwrap_err();
        assert!(error.contains("`at`"), "{error}");
        assert_eq!(
            check("t.c", AnchorSurface::Cli).unwrap_err(),
            INVALID_T_C,
            "one message, not wrapped in another error's prefix"
        );
    }

    /// What `parse_location` says of `t.c`, with no error's prefix.
    const INVALID_T_C: &str = "invalid location `t.c`: expected `file:line`, e.g. `parser.c:8`";

    #[test]
    fn a_call_site_that_is_no_file_line_is_refused_with_one_message() {
        let session = crate::testing::session_from(&[("a", "b")]);
        let error = anchor_from_parts(
            &session,
            &parts(Some("t.c"), Some(0), false, None, None),
            AnchorSurface::Cli,
        )
        .unwrap_err();
        assert_eq!(error, INVALID_T_C);
    }
}
