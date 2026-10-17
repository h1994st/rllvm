//! The `guards` query: the comparisons, clamps, masks and moduli that
//! constrain a value at a call-site argument, a call result, or a named
//! local.
//!
//! This reports evidence and never judges: an empty `guards` list means the
//! trace found nothing constraining the value, not that the value is
//! unbounded.

use serde::{Deserialize, Serialize};

use rllvm_core::error::Error;

use crate::ir::FunctionIr;
use crate::{CallSiteId, FunctionId, GuardKind, Session, SourceLocation};

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
    use crate::FunctionId;
    use crate::ir::BlockIr;

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
