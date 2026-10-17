//! The `guards` query: the comparisons, clamps, masks and moduli that
//! constrain a value at a call-site argument, a call result, or a named
//! local.
//!
//! This reports evidence and never judges: an empty `guards` list means the
//! trace found nothing constraining the value, not that the value is
//! unbounded.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use rllvm_core::error::Error;

use crate::bind::collapse_odr_duplicates;
use crate::extract::read_module_ir;
use crate::ir::{
    CallIr, CastOp, FunctionIr, GuardOpRaw, ModuleIr, OpcodeIr, Operand, ValueId, ValueNode,
    int_predicate,
};
use crate::overlay::function_label;
use crate::slice::promote_anchor_module;
use crate::{CallSiteId, CallTarget, FunctionId, GuardKind, Linkage, Session, SourceLocation};

/// The value whose constraints are asked for.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardAnchor {
    /// Operand `operand` of the call at `site`.
    Argument { site: CallSiteId, operand: usize },
    /// The result of the call at `site`.
    Return { site: CallSiteId },
    /// The source variable `name` in `function`.
    Local { function: String, name: String },
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

/// One instruction that constrains the anchored value.
#[derive(Clone, Debug, Serialize)]
pub struct GuardSite {
    pub op: GuardKind,
    /// Reads with the traced value on the left, `value ult bound`, however
    /// the instruction orders its operands. A saturating subtraction from
    /// the bound, `bound - value`, has a `.rev` suffix: `usub.sat.rev`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
    pub relation: GuardRelation,
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
pub struct Crossing {
    pub site: CallSiteId,
    pub direction: CrossingDir,
    pub function: FunctionId,
    pub position: String,
}

/// The `results` of a `guards` query.
#[derive(Clone, Debug, Default, Serialize)]
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

/// Casts that keep an integer's value, only resizing it.
const RESIZING_CASTS: [CastOp; 3] = [CastOp::Trunc, CastOp::ZExt, CastOp::SExt];

/// Intrinsics, by name prefix, whose result is read from memory or a
/// register rather than computed from their arguments.
const READING_INTRINSICS: [&str; 9] = [
    "llvm.masked.load.",
    "llvm.masked.gather.",
    "llvm.masked.expandload.",
    "llvm.vp.load.",
    "llvm.vp.gather.",
    "llvm.experimental.vp.strided.load.",
    "llvm.load.relative.",
    "llvm.read_register.",
    "llvm.read_volatile_register.",
];

/// How intrinsic names begin. An intrinsic's result is computed from its
/// arguments, so the trace reads through it rather than crossing into it.
const INTRINSIC_PREFIX: &str = "llvm.";

/// The position a value crossing out of a callee holds.
const RETURN_POSITION: &str = "return";

/// What separates the reasons a trace was lost.
const LOST_SEPARATOR: &str = "; ";

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
    match resolve_anchor(&ir, session, anchor)? {
        Resolved::Ambiguous(values) => {
            let name = match anchor {
                GuardAnchor::Local { name, .. } => name.as_str(),
                GuardAnchor::Argument { .. } | GuardAnchor::Return { .. } => "?",
            };
            lost.push(format!(
                "local `{name}` in {} is bound to more than one value over its lifetime ({} of \
                 them SSA values), so none is traced",
                function_label(&function),
                values.len()
            ));
        }
        Resolved::One {
            function: index,
            value,
        } => {
            let body = &ir.functions[index];
            let trace = Trace {
                session,
                body,
                function: &function,
                scope: scope.as_ref(),
                doms: dominators(body),
            };
            let at = use_point(body, session, anchor, value)?;
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

/// The functions on a path from `from` to `function`: the callers a
/// parameter may cross from when the trace is bounded to that slice. An
/// error when `function` is on no such path.
fn caller_scope(
    session: &Session,
    from: &str,
    function: &FunctionId,
) -> Result<HashSet<FunctionId>, Error> {
    if session.resolve(from).is_none() {
        return Err(Error::InvalidArguments(format!(
            "no function named {from:?} in the catalog to bound the trace from"
        )));
    }
    let scope: HashSet<FunctionId> = session
        .slice_with(from, &function.symbol, None)
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
    crossings: Vec<Crossing>,
    lost: Vec<String>,
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
                pending.extend(operands.iter().filter_map(|operand| match operand {
                    Operand::Value(value) => Some(*value),
                    _ => None,
                }));
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
                            Some(callee)
                                if READING_INTRINSICS
                                    .iter()
                                    .any(|prefix| callee.symbol.starts_with(prefix)) =>
                            {
                                walk.lost.push(format!(
                                    "`{}` reads the value from memory or a register{}, and what \
                                     was stored there is not traced",
                                    callee.symbol,
                                    place(node)
                                ));
                            }
                            Some(callee) if callee.symbol.starts_with(INTRINSIC_PREFIX) => {
                                follow(&call.args);
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
                OpcodeIr::Load => walk.lost.push(format!(
                    "the value is loaded from memory{}, and what was stored there is not traced",
                    place(node)
                )),
                OpcodeIr::Other => walk.lost.push(format!(
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
    fn guard_sites(&self, walk: &Walk, value: ValueId, at: ValueId) -> Vec<GuardSite> {
        let mut sites = walk.defining.clone();
        let mut folded = walk.folded.clone();
        let downstream = self.downstream(value);
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
            if node.opcode == OpcodeIr::Phi {
                if let Some((mut site, compare)) = self.phi_clamp(node) {
                    site.relation = GuardRelation::OnPath;
                    sites.insert(id, site);
                    folded.insert(compare);
                }
            } else if let Some(raw) = &node.guard {
                sites.insert(
                    id,
                    surfaced(raw, side == 1, node.location.clone(), GuardRelation::OnPath),
                );
                if let (OpcodeIr::Select, Some(Operand::Value(compare))) =
                    (node.opcode, node.operands.first())
                {
                    folded.insert(*compare);
                }
            }
        }
        for (id, node) in self.body.values.iter().enumerate() {
            let id = id as ValueId;
            if node.opcode != OpcodeIr::ICmp || sites.contains_key(&id) || folded.contains(&id) {
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
            sites.insert(
                id,
                surfaced(raw, side == 1, node.location.clone(), relation),
            );
        }
        sites.into_values().collect()
    }

    /// `value`, and what it is only widened or narrowed into downstream:
    /// a guard on those constrains it too.
    fn downstream(&self, value: ValueId) -> BTreeSet<ValueId> {
        let mut values = BTreeSet::from([value]);
        let mut grew = true;
        while grew {
            grew = false;
            for (id, node) in self.body.values.iter().enumerate() {
                if let (OpcodeIr::Cast { op }, Some(Operand::Value(from))) =
                    (node.opcode, node.operands.first())
                    && RESIZING_CASTS.contains(&op)
                    && values.contains(from)
                {
                    grew |= values.insert(id as ValueId);
                }
            }
        }
        values
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
        let bound = [a, b].into_iter().find_map(|operand| match operand {
            Operand::Const(constant) => Some(constant.value.clone()),
            _ => None,
        });
        let site = GuardSite {
            op: GuardKind::Clamp,
            predicate: Some(predicate.clamps?[picks].to_string()),
            bound,
            location: node.location.clone().or_else(|| phi.location.clone()),
            relation: GuardRelation::Defines,
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
        let calls_traced =
            |callee: &FunctionId| definition_of(self.session, callee) == *self.function;
        self.session
            .callers(&self.function.symbol)
            .into_iter()
            .filter(|site| match &site.target {
                CallTarget::Direct { callee } => calls_traced(callee),
                CallTarget::Indirect {
                    llvm_target_bound: Some(bound),
                    ..
                } => bound.iter().any(calls_traced),
                _ => false,
            })
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
        predicate: predicate.map(str::to_string),
        bound: raw.bound.clone(),
        location,
        relation,
    }
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
    /// Value `value` of `ir.functions[function]`.
    One { function: usize, value: ValueId },
    /// A local bound to more than one value over its lifetime: each SSA
    /// value it is bound to, in binding order. A binding to a constant, or to
    /// nothing traceable, counts toward the ambiguity but is not listed.
    Ambiguous(Vec<ValueId>),
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
            "{name:?} names {} defined functions ({}); a local needs exactly one",
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
    let id = anchor_function(session, anchor)?;
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
            Ok(Resolved::One { function, value })
        }
        GuardAnchor::Local { name, .. } => resolve_local(body, function, name),
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
/// `ir.functions[function]`. Bindings to one value are one value however
/// often they recur; bindings to several are reported, never chosen
/// between.
fn resolve_local(body: &FunctionIr, function: usize, name: &str) -> Result<Resolved, Error> {
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
    let label = function_label(&body.id);
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
    match (bound.len(), values.as_slice()) {
        (1, &[value]) => Ok(Resolved::One { function, value }),
        (_, []) => {
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
        _ => Ok(Resolved::Ambiguous(values)),
    }
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

fn site_label(site: &CallSiteId) -> String {
    format!(
        "site {}:{} in {}",
        site.block_index,
        site.instruction_index,
        function_label(&site.function)
    )
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
        ir::{BinaryOp, BlockIr, OpcodeIr, Operand, ValueNode},
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
        let Resolved::One { function, value } = *resolved else {
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

    #[test]
    fn an_argument_resolves_to_the_value_passed() {
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
    }

    #[test]
    fn an_argument_that_is_no_value_is_refused_naming_it() {
        let scratch = tempfile::tempdir().unwrap();
        let (session, ir) = promoted(&scratch, ARGUMENT_SOURCE, &["-O0", "-g"], "f");
        let sites = sites_calling(&session, "f", "sink");

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
        let Resolved::Ambiguous(values) = resolve_anchor(&ir, &session, &local("g", "x")).unwrap()
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
        }
    }

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
            assert_eq!(
                sites(&r),
                [
                    (
                        GuardKind::Compare,
                        Some("ugt"),
                        Some("100"),
                        GuardRelation::OnPath
                    ),
                    (
                        GuardKind::Clamp,
                        Some("umin"),
                        Some("16"),
                        GuardRelation::OnPath
                    ),
                ],
                "{optimization}: {r:?}"
            );
            assert_eq!(crossings(&r), [("f", CrossingDir::From, "get", "return")]);
        }
    }

    #[test]
    fn a_local_surfaces_the_guards_on_it() {
        let scratch = tempfile::tempdir().unwrap();
        let session = session(&scratch, DOMINATING_SOURCE, &["-O0", "-g"]);
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
}
