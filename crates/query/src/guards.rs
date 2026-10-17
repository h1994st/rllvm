//! The `guards` query: the comparisons, clamps, masks and moduli that
//! constrain a value at a call-site argument, a call result, or a named
//! local.
//!
//! This reports evidence and never judges: an empty `guards` list means the
//! trace found nothing constraining the value, not that the value is
//! unbounded.

use serde::{Deserialize, Serialize};

use rllvm_core::error::Error;

use crate::bind::collapse_odr_duplicates;
use crate::ir::{CallIr, FunctionIr, ModuleIr, Operand, ValueId};
use crate::overlay::function_label;
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
#[derive(Clone, Copy, Debug, Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
    pub relation: GuardRelation,
}

/// Which way a trace crossed a call boundary.
#[derive(Clone, Copy, Debug, Serialize)]
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
    /// Why the trace stopped early, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<String>,
}

impl GuardsResult {
    pub(crate) fn is_empty(&self) -> bool {
        self.guards.is_empty() && self.crossings.is_empty()
    }
}

/// Trace the guards of `anchor`, optionally starting from `from`.
///
/// Scaffold: returns an empty result until the analysis lands.
pub fn guards(
    _session: &Session,
    _anchor: &GuardAnchor,
    _from: Option<&str>,
) -> Result<GuardsResult, Error> {
    Ok(GuardsResult::default())
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the guard trace starts from it, which lands next")
)]
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the guard trace starts from it, which lands next")
)]
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
            let (value, _) = promoted_call(body, session, site)?;
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
    let mut peers: Vec<&CallSiteId> = session
        .call_sites()
        .iter()
        .filter(|peer| peer.id.function == site.function)
        .filter(|peer| same_place(peer.location.as_ref(), location))
        .filter(|peer| match &peer.target {
            CallTarget::Intrinsic { name } => !deleted_by_promotion(name),
            _ => true,
        })
        .map(|peer| &peer.id)
        .collect();
    peers.sort_by_key(|peer| (peer.block_index, peer.instruction_index));
    let missing = || {
        Error::InvalidArguments(format!(
            "the call at {} has no counterpart in the promoted module",
            site_label(site)
        ))
    };
    let ordinal = peers
        .iter()
        .position(|peer| *peer == site)
        .ok_or_else(missing)?;
    body.values
        .iter()
        .enumerate()
        .filter_map(|(id, node)| Some((id as ValueId, node, node.call.as_ref()?)))
        .filter(|(_, node, _)| same_place(node.location.as_ref(), location))
        .filter(|(_, _, call)| {
            !call
                .callee
                .as_ref()
                .is_some_and(|callee| deleted_by_promotion(&callee.symbol))
        })
        .nth(ordinal)
        .map(|(id, _, call)| (id, call))
        .ok_or_else(missing)
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the guard trace, which lands next")
)]
pub(crate) struct Doms {
    /// `idom[b]` is `b`'s immediate dominator; the entry is its own. `None`
    /// for a block unreachable from the entry.
    idom: Vec<Option<usize>>,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the guard trace, which lands next")
)]
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the guard trace, which lands next")
)]
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
        testing::compiled_session,
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
        let bytes = promote_anchor_module(&session, &anchor).unwrap();
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
