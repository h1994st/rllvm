//! The `guards` query: the comparisons, clamps, masks and moduli that
//! constrain a value at a call-site argument, a call result, or a named
//! local.
//!
//! This reports evidence and never judges: an empty `guards` list means the
//! trace found nothing constraining the value, not that the value is
//! unbounded.

use serde::{Deserialize, Serialize};

use rllvm_core::error::Error;

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
