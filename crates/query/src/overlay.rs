//! The call-graph overlay: agent-authored edges kept in a sidecar file.
//!
//! The catalog is never touched. An agent that resolves an indirect call --
//! by reading the code, say, from a `resolution-candidates` group -- records
//! a hypothesis here, as one JSON line per record after a header that binds
//! the file to one build of one catalog. rllvm-query never judges a record:
//! it checks only that the record is structurally grounded, i.e. that it
//! names a real function and attaches to an indirect call site LLVM left
//! unbounded. A bounded site already has a sound target set, and an agent
//! edge may never widen it.
//!
//! The file is append-only between compactions, so a crash or a concurrent
//! reader never sees an earlier line rewritten. Every line is validated when
//! the file is folded, and any bad one fails the load naming its line: a
//! record silently skipped would change what the overlay claims.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use rllvm_core::{catalog::hash_bytes, error::Error};

use crate::{
    facts::{CallSiteFact, CallSiteId, CallTarget, FieldRef, FunctionId, ModuleAnalysis},
    index::{NameMatch, NameResolution, Session},
};

/// The `v` of the header line this version writes and reads.
pub const OVERLAY_VERSION: u32 = 1;

/// The only `source` an `add` may carry, and the one every stored edge has.
const AGENT_SOURCE: &str = "agent";

/// Every `op` this version reads. Any other is named in the load error,
/// never skipped: `annotate` is reserved for a later version.
const KNOWN_OPS: &[&str] = &["add", "verify", "retract"];

/// Appended to the catalog's file stem for the default overlay path.
const DEFAULT_OVERLAY_SUFFIX: &str = "overlay.jsonl";

fn agent() -> String {
    AGENT_SOURCE.to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirmed,
    Refuted,
    Inconclusive,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verdict::Confirmed => "confirmed",
            Verdict::Refuted => "refuted",
            Verdict::Inconclusive => "inconclusive",
        })
    }
}

/// What an agent edge is keyed by: a field-to-target pattern that applies
/// to every unresolved site dispatching through the field, or one site.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EdgeKey {
    Field { via_field: FieldRef, to: FunctionId },
    Site { site: CallSiteId, to: FunctionId },
}

impl EdgeKey {
    /// The unresolved sites this edge attaches to: every one dispatching
    /// through the field, or the one site while it stays unresolved.
    pub(crate) fn sites<'s>(&self, session: &'s Session) -> Vec<&'s CallSiteFact> {
        match self {
            EdgeKey::Field { via_field, .. } => session.unresolved_sites_through(via_field),
            EdgeKey::Site { site, .. } => session
                .call_site(site)
                .filter(|site| is_unresolved(site))
                .into_iter()
                .collect(),
        }
    }
}

/// `symbol [module]`: both halves of a function's identity.
fn function_label(id: &FunctionId) -> String {
    format!("{} [{}]", id.symbol, id.module_id)
}

impl fmt::Display for EdgeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdgeKey::Field { via_field, to } => {
                write!(f, "{via_field} -> {}", function_label(to))
            }
            EdgeKey::Site { site, to } => write!(
                f,
                "{} site {}:{} -> {}",
                function_label(&site.function),
                site.block_index,
                site.instruction_index,
                function_label(to)
            ),
        }
    }
}

/// `to` as written in a record: a full id, or a symbol that must name
/// exactly one definition. Always stored resolved.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TargetSpec {
    Id(FunctionId),
    Symbol(String),
}

/// One JSONL line after the header.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    Add {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_field: Option<FieldRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<CallSiteId>,
        to: TargetSpec,
        confidence: Confidence,
        provenance: Vec<String>,
        /// Forced to "agent" on write; any other value is rejected.
        #[serde(default = "agent")]
        source: String,
    },
    Verify {
        edge: EdgeKey,
        tool: String,
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    Retract {
        edge: EdgeKey,
        reason: String,
    },
}

impl std::str::FromStr for Record {
    type Err = Error;

    /// One record as a line of JSON. An unknown `op` is named as such
    /// rather than as serde's unknown variant.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        parse_record(line).map_err(Error::InvalidArguments)
    }
}

/// [`Record::from_str`], with the reason bare so a load can prefix its
/// `path:line`.
fn parse_record(line: &str) -> Result<Record, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| format!("not a JSON record: {error}"))?;
    if let Some(op) = value.get("op").and_then(serde_json::Value::as_str)
        && !KNOWN_OPS.contains(&op)
    {
        return Err(format!("unknown op {op:?}"));
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Verification {
    pub tool: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OverlayEdge {
    pub key: EdgeKey,
    pub confidence: Confidence,
    pub provenance: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

impl OverlayEdge {
    fn is_refuted(&self) -> bool {
        self.verification
            .as_ref()
            .is_some_and(|verification| verification.verdict == Verdict::Refuted)
    }
}

/// The first line of every overlay file.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    v: u32,
    fingerprint: String,
}

/// The folded overlay, bound to one build of one catalog.
pub struct Overlay {
    /// Where `save` and `compact` write; `None` for one never saved.
    path: Option<PathBuf>,
    /// [`fingerprint`] of the session this overlay was opened against.
    fingerprint: String,
    edges: BTreeMap<EdgeKey, OverlayEdge>,
    /// Applied records not yet appended to the file, `to` resolved.
    pending: Vec<Record>,
    /// Whether the file exists with this overlay's header, so `save`
    /// appends rather than creating it.
    on_disk: bool,
}

impl Overlay {
    /// An empty overlay for `session`, to be saved at `path` (or never, for `None`).
    pub fn empty(session: &Session, path: Option<PathBuf>) -> Result<Overlay, Error> {
        Ok(Overlay {
            path,
            fingerprint: fingerprint(session)?,
            edges: BTreeMap::new(),
            pending: Vec::new(),
            on_disk: false,
        })
    }

    /// Reads and folds `path` against `session`. A missing file is an empty
    /// overlay bound to that path; every other problem is an error naming
    /// the path and the 1-based line.
    pub fn open(session: &Session, path: &Path) -> Result<Overlay, Error> {
        let mut overlay = Overlay::empty(session, Some(path.to_path_buf()))?;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(overlay),
            Err(error) => return Err(Error::file(path, error)),
        };
        let at = |line: usize, reason: String| {
            Error::InvalidArguments(format!("{}:{line}: {reason}", path.display()))
        };

        let mut lines = text
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line));
        let header: Header = lines
            .next()
            .and_then(|(_, line)| serde_json::from_str(line).ok())
            .ok_or_else(|| {
                at(
                    1,
                    format!(
                        "no overlay header; the first line must be \
                         {{\"v\":{OVERLAY_VERSION},\"fingerprint\":\"...\"}}"
                    ),
                )
            })?;
        if header.v != OVERLAY_VERSION {
            return Err(at(
                1,
                format!(
                    "overlay version {}; this rllvm-query reads version {OVERLAY_VERSION}",
                    header.v
                ),
            ));
        }
        if header.fingerprint != overlay.fingerprint {
            return Err(at(
                1,
                format!(
                    "recorded against a different build of this catalog (overlay {}, catalog {})",
                    header.fingerprint, overlay.fingerprint
                ),
            ));
        }
        for (number, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let record = parse_record(line).map_err(|reason| at(number, reason))?;
            apply(&mut overlay.edges, session, record).map_err(|reason| at(number, reason))?;
        }
        overlay.on_disk = true;
        Ok(overlay)
    }

    /// Validates every record against `session` first, then applies all of
    /// them, or none. Applied records are pending until `save`.
    ///
    /// Records are validated in order against the state the earlier ones
    /// leave, so one batch may add an edge and verify it.
    pub fn record(&mut self, session: &Session, records: Vec<Record>) -> Result<(), Error> {
        let mut edges = self.edges.clone();
        let mut applied = Vec::with_capacity(records.len());
        for (index, record) in records.into_iter().enumerate() {
            applied.push(apply(&mut edges, session, record).map_err(|reason| {
                Error::InvalidArguments(format!("record {}: {reason}", index + 1))
            })?);
        }
        self.edges = edges;
        self.pending.extend(applied);
        Ok(())
    }

    /// Appends pending records (writing the header first for a new file).
    /// Returns how many were written.
    pub fn save(&mut self) -> Result<usize, Error> {
        if self.pending.is_empty() {
            return Ok(0);
        }
        let path = self.path.clone().ok_or_else(|| {
            Error::InvalidArguments("this overlay has no file to save to".to_string())
        })?;
        let mut file = if self.on_disk {
            append_to(&path)?
        } else {
            // `create_new`, not `create`: a file that appeared since `open`
            // has its own header, and a second one mid-file would corrupt it.
            let mut file = OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| Error::file(&path, error))?;
            let header = json_line(&Header {
                v: OVERLAY_VERSION,
                fingerprint: self.fingerprint.clone(),
            })?;
            file.write_all(header.as_bytes())
                .map_err(|error| Error::file(&path, error))?;
            self.on_disk = true;
            file
        };

        for (written, record) in self.pending.iter().enumerate() {
            let line = json_line(record)?;
            if let Err(error) = file.write_all(line.as_bytes()) {
                // What reached the file is no longer pending: writing it
                // again on a retry would replay a retract that already ran.
                self.pending.drain(..written);
                return Err(Error::file(&path, error));
            }
        }
        let written = self.pending.len();
        self.pending.clear();
        Ok(written)
    }

    /// Rewrites the file as the header plus one `add` (and one `verify`) per
    /// current edge, atomically (temp file in the same directory, then
    /// rename). Refuses while records are pending.
    pub fn compact(&mut self) -> Result<(), Error> {
        if !self.pending.is_empty() {
            return Err(Error::InvalidArguments(format!(
                "{} unsaved record(s); save them before compacting",
                self.pending.len()
            )));
        }
        let path = self.path.clone().ok_or_else(|| {
            Error::InvalidArguments("this overlay has no file to compact".to_string())
        })?;
        let mut text = json_line(&Header {
            v: OVERLAY_VERSION,
            fingerprint: self.fingerprint.clone(),
        })?;
        for edge in self.edges.values() {
            text.push_str(&json_line(&add_record(
                &edge.key,
                edge.confidence,
                edge.provenance.clone(),
            ))?);
            if let Some(verification) = &edge.verification {
                text.push_str(&json_line(&Record::Verify {
                    edge: edge.key.clone(),
                    tool: verification.tool.clone(),
                    verdict: verification.verdict,
                    at: verification.at.clone(),
                })?);
            }
        }

        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(directory)
            .map_err(|error| Error::file(directory, error))?;
        temporary
            .write_all(text.as_bytes())
            .map_err(|error| Error::file(temporary.path(), error))?;
        temporary
            .persist(&path)
            .map_err(|error| Error::file(&path, error.error))?;
        self.on_disk = true;
        Ok(())
    }

    pub fn edges(&self) -> impl Iterator<Item = &OverlayEdge> {
        self.edges.values()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn summary(&self, session: &Session) -> OverlaySummary {
        let mut covered: BTreeSet<&CallSiteId> = BTreeSet::new();
        let edges = self
            .edges
            .values()
            .map(|edge| {
                let sites = edge.key.sites(session);
                if !edge.is_refuted() {
                    covered.extend(sites.iter().map(|site| &site.id));
                }
                SummaryEdge {
                    edge: edge.clone(),
                    sites: sites.len(),
                }
            })
            .collect();
        OverlaySummary {
            path: self.path.clone(),
            fingerprint: self.fingerprint.clone(),
            pending: self.pending.len(),
            edges,
            coverage: OverlayCoverage {
                unresolved_sites: session
                    .call_sites()
                    .iter()
                    .filter(|site| is_unresolved(site))
                    .count(),
                covered_sites: covered.len(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OverlaySummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub fingerprint: String,
    pub pending: usize,
    pub edges: Vec<SummaryEdge>,
    pub coverage: OverlayCoverage,
}

#[derive(Debug, Serialize)]
pub struct SummaryEdge {
    #[serde(flatten)]
    pub edge: OverlayEdge,
    /// Unresolved sites this edge attaches to: every site through the field,
    /// or the one site.
    pub sites: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OverlayCoverage {
    pub unresolved_sites: usize,
    /// Unresolved sites at least one non-refuted edge attaches to.
    pub covered_sites: usize,
}

/// sha256 (hex) over the sorted `(module_id, content_sha256)` pairs of the
/// session's analyzed modules, each pair written `id\0hash\n`. Errors when an
/// analyzed module has no content hash: its call-site ids cannot be pinned.
pub fn fingerprint(session: &Session) -> Result<String, Error> {
    let mut pairs = Vec::new();
    for module in session
        .modules()
        .iter()
        .filter(|module| module.status == ModuleAnalysis::Analyzed)
    {
        let hash = module.content_sha256.as_deref().ok_or_else(|| {
            Error::InvalidArguments(format!(
                "module {} has no content hash, so an overlay cannot be bound to this build \
                 of the catalog",
                module.id
            ))
        })?;
        pairs.push((module.id.as_str(), hash));
    }
    pairs.sort_unstable();
    let mut bytes = Vec::new();
    for (id, hash) in pairs {
        bytes.extend_from_slice(id.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(hash.as_bytes());
        bytes.push(b'\n');
    }
    Ok(hash_bytes(&bytes))
}

/// `<catalog stem>.overlay.jsonl` beside the catalog.
pub fn default_overlay_path(catalog: &Path) -> PathBuf {
    let stem = catalog
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    catalog.with_file_name(format!("{stem}.{DEFAULT_OVERLAY_SUFFIX}"))
}

/// An indirect call site LLVM left without a bound: the only kind an
/// overlay edge may attach to.
fn is_unresolved(site: &CallSiteFact) -> bool {
    matches!(
        site.target,
        CallTarget::Indirect {
            llvm_target_bound: None,
            ..
        }
    )
}

/// Opens an existing overlay for appending, first ending a final line a
/// hand edit left without its newline, so the next record starts a line of
/// its own.
fn append_to(path: &Path) -> Result<std::fs::File, Error> {
    let fail = |error| Error::file(path, error);
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
        .map_err(fail)?;
    if file.seek(SeekFrom::End(0)).map_err(fail)? > 0 {
        file.seek(SeekFrom::End(-1)).map_err(fail)?;
        let mut last = [0u8];
        file.read_exact(&mut last).map_err(fail)?;
        if last[0] != b'\n' {
            file.write_all(b"\n").map_err(fail)?;
        }
    }
    Ok(file)
}

/// One value as a newline-terminated JSON line.
fn json_line(value: &impl Serialize) -> Result<String, Error> {
    let mut line =
        serde_json::to_string(value).map_err(|error| Error::InvalidArguments(error.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// The `add` that recreates the edge at `key`, as stored: `to` resolved,
/// `source` the agent.
fn add_record(key: &EdgeKey, confidence: Confidence, provenance: Vec<String>) -> Record {
    let (via_field, site, to) = match key {
        EdgeKey::Field { via_field, to } => (Some(via_field.clone()), None, to),
        EdgeKey::Site { site, to } => (None, Some(site.clone()), to),
    };
    Record::Add {
        via_field,
        site,
        to: TargetSpec::Id(to.clone()),
        confidence,
        provenance,
        source: agent(),
    }
}

/// Validates `record` against `session` and the current `edges`, applies
/// it, and returns it as it is stored. The reason is bare, for the caller
/// to prefix with where the record came from.
fn apply(
    edges: &mut BTreeMap<EdgeKey, OverlayEdge>,
    session: &Session,
    record: Record,
) -> Result<Record, String> {
    match record {
        Record::Add {
            via_field,
            site,
            to,
            confidence,
            provenance,
            source,
        } => {
            if source != AGENT_SOURCE {
                return Err(format!("source must be \"{AGENT_SOURCE}\", not {source:?}"));
            }
            if provenance.is_empty() {
                return Err("an add needs at least one provenance entry".to_string());
            }
            let to = resolve_target(session, &to)?;
            let key = match (via_field, site) {
                (Some(via_field), None) => {
                    if session.unresolved_sites_through(&via_field).is_empty() {
                        return Err(format!(
                            "no unresolved indirect call site dispatches through {via_field}"
                        ));
                    }
                    EdgeKey::Field { via_field, to }
                }
                (None, Some(site)) => {
                    check_site(session, &site)?;
                    EdgeKey::Site { site, to }
                }
                _ => return Err("an add names exactly one of via_field and site".to_string()),
            };
            match edges.get_mut(&key) {
                Some(edge) => {
                    edge.confidence = confidence;
                    edge.provenance = provenance.clone();
                }
                None => {
                    edges.insert(
                        key.clone(),
                        OverlayEdge {
                            key: key.clone(),
                            confidence,
                            provenance: provenance.clone(),
                            verification: None,
                        },
                    );
                }
            }
            Ok(add_record(&key, confidence, provenance))
        }
        Record::Verify {
            edge,
            tool,
            verdict,
            at,
        } => {
            let current = edges
                .get_mut(&edge)
                .ok_or_else(|| format!("no current edge {edge}"))?;
            current.verification = Some(Verification {
                tool: tool.clone(),
                verdict,
                at: at.clone(),
            });
            Ok(Record::Verify {
                edge,
                tool,
                verdict,
                at,
            })
        }
        Record::Retract { edge, reason } => {
            edges
                .remove(&edge)
                .ok_or_else(|| format!("no current edge {edge}"))?;
            Ok(Record::Retract { edge, reason })
        }
    }
}

/// The one function `to` names. A symbol is matched exactly, never by its
/// demangled reading or an identifier search, and must leave exactly one
/// candidate once definitions are preferred over declarations.
fn resolve_target(session: &Session, to: &TargetSpec) -> Result<FunctionId, String> {
    let symbol = match to {
        TargetSpec::Id(id) => {
            return session
                .function(id)
                .map(|function| function.id.clone())
                .ok_or_else(|| format!("no function {}", function_label(id)));
        }
        TargetSpec::Symbol(symbol) => symbol,
    };
    let Some(NameResolution {
        matched: NameMatch::Mangled,
        ids,
    }) = session.resolve(symbol)
    else {
        return Err(format!(
            "no function has the symbol {symbol:?}; `to` takes an exact symbol"
        ));
    };
    let definitions: Vec<FunctionId> = ids
        .iter()
        .filter(|id| {
            session
                .function(id)
                .is_some_and(|function| function.is_definition)
        })
        .cloned()
        .collect();
    let mut candidates = if definitions.is_empty() {
        ids
    } else {
        definitions
    };
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(format!(
            "{symbol:?} names {} functions, in modules {}; give `to` as \
             {{\"module_id\": ..., \"symbol\": ...}}",
            candidates.len(),
            candidates
                .iter()
                .map(|id| id.module_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A site an agent edge may attach to: present, indirect and unbounded.
fn check_site(session: &Session, site: &CallSiteId) -> Result<(), String> {
    match session.call_site(site).map(|fact| &fact.target) {
        None => Err(format!(
            "no call site {}:{} in {}",
            site.block_index,
            site.instruction_index,
            function_label(&site.function)
        )),
        Some(CallTarget::Indirect {
            llvm_target_bound: None,
            ..
        }) => Ok(()),
        Some(CallTarget::Indirect { .. }) => {
            Err("the site has an LLVM bound; an agent edge may not widen it".to_string())
        }
        Some(_) => Err("not an indirect call site".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rllvm_core::error::Error;

    use super::*;
    use crate::{
        facts::{CallSiteId, FieldRef, FunctionId, ProgramFacts},
        index::Session,
        testing::facts_with_overlay_sites,
    };

    fn ops(offset: u64) -> FieldRef {
        FieldRef {
            record: "ops".into(),
            offset,
        }
    }

    fn id(module: &str, symbol: &str) -> FunctionId {
        FunctionId {
            module_id: module.into(),
            symbol: symbol.into(),
        }
    }

    fn session() -> Session {
        Session::new(facts_with_overlay_sites(), Vec::new())
    }

    /// The one call site `caller` makes.
    fn site_in(session: &Session, caller: &str) -> CallSiteId {
        let sites: Vec<_> = session
            .call_sites()
            .iter()
            .filter(|site| site.id.function.symbol == caller)
            .collect();
        assert_eq!(sites.len(), 1, "{caller}");
        sites[0].id.clone()
    }

    fn add(via_field: Option<FieldRef>, site: Option<CallSiteId>, to: TargetSpec) -> Record {
        Record::Add {
            via_field,
            site,
            to,
            confidence: Confidence::High,
            provenance: vec!["init: o->on_event = h3".into()],
            source: "agent".into(),
        }
    }

    fn add_field(offset: u64, to: &str) -> Record {
        add(Some(ops(offset)), None, TargetSpec::Symbol(to.into()))
    }

    fn field_key(to: &str) -> EdgeKey {
        EdgeKey::Field {
            via_field: ops(8),
            to: id("mod_a", to),
        }
    }

    fn overlay_path(scratch: &tempfile::TempDir) -> PathBuf {
        scratch.path().join("catalog.overlay.jsonl")
    }

    /// The edges as JSON: what two overlays must agree on to hold one state.
    fn state(overlay: &Overlay) -> serde_json::Value {
        serde_json::to_value(overlay.edges().collect::<Vec<_>>()).unwrap()
    }

    fn message(error: Error) -> String {
        match error {
            Error::InvalidArguments(message) => message,
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    fn record_error(session: &Session, records: Vec<Record>) -> String {
        let mut overlay = Overlay::empty(session, None).unwrap();
        message(overlay.record(session, records).unwrap_err())
    }

    fn open_error(session: &Session, path: &Path) -> String {
        message(Overlay::open(session, path).err().expect("open must fail"))
    }

    fn header(session: &Session) -> String {
        format!(
            "{{\"v\":1,\"fingerprint\":\"{}\"}}\n",
            fingerprint(session).unwrap()
        )
    }

    #[test]
    fn a_field_edge_folds_and_survives_a_reopen() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let mut overlay = Overlay::open(&session, &path).unwrap();
        assert_eq!(overlay.edges().count(), 0, "a missing file is empty");
        assert_eq!(overlay.path(), Some(path.as_path()));

        overlay.record(&session, vec![add_field(8, "h3")]).unwrap();
        assert_eq!(overlay.pending(), 1);
        assert_eq!(overlay.save().unwrap(), 1);
        assert_eq!(overlay.pending(), 0);

        let reopened = Overlay::open(&session, &path).unwrap();
        assert_eq!(reopened.pending(), 0);
        assert_eq!(state(&reopened), state(&overlay));
        let edges: Vec<_> = reopened.edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].key, field_key("h3"));

        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines[0], header(&session).trim_end());
        let stored: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            stored["to"],
            serde_json::json!({ "module_id": "mod_a", "symbol": "h3" }),
            "a symbol is stored resolved"
        );
        assert_eq!(stored["source"], "agent");
    }

    #[test]
    fn appending_never_rewrites_earlier_lines() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let mut overlay = Overlay::open(&session, &path).unwrap();
        overlay.record(&session, vec![add_field(8, "h3")]).unwrap();
        overlay.save().unwrap();
        let first = std::fs::read(&path).unwrap();

        overlay.record(&session, vec![add_field(8, "h1")]).unwrap();
        assert_eq!(overlay.save().unwrap(), 1);
        let second = std::fs::read(&path).unwrap();
        assert!(second.len() > first.len());
        assert_eq!(&second[..first.len()], first.as_slice());
        assert_eq!(Overlay::open(&session, &path).unwrap().edges().count(), 2);
    }

    #[test]
    fn an_append_after_a_hand_edit_without_a_final_newline_starts_its_own_line() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let mut overlay = Overlay::open(&session, &path).unwrap();
        overlay.record(&session, vec![add_field(8, "h3")]).unwrap();
        overlay.save().unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, written.trim_end()).unwrap();

        let mut overlay = Overlay::open(&session, &path).unwrap();
        overlay.record(&session, vec![add_field(8, "h1")]).unwrap();
        overlay.save().unwrap();
        assert_eq!(Overlay::open(&session, &path).unwrap().edges().count(), 2);
    }

    #[test]
    fn a_retract_removes_and_a_verify_annotates() {
        let session = session();
        let mut overlay = Overlay::empty(&session, None).unwrap();
        overlay
            .record(
                &session,
                vec![
                    add_field(8, "h3"),
                    add_field(8, "h1"),
                    Record::Verify {
                        edge: field_key("h3"),
                        tool: "reviewer".into(),
                        verdict: Verdict::Confirmed,
                        at: None,
                    },
                    Record::Retract {
                        edge: field_key("h1"),
                        reason: "never stored".into(),
                    },
                ],
            )
            .unwrap();
        let edges: Vec<_> = overlay.edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].key, field_key("h3"));
        let verification = edges[0].verification.as_ref().unwrap();
        assert_eq!(verification.verdict, Verdict::Confirmed);
        assert_eq!(verification.tool, "reviewer");

        let error = message(
            overlay
                .record(
                    &session,
                    vec![Record::Retract {
                        edge: field_key("h1"),
                        reason: "again".into(),
                    }],
                )
                .unwrap_err(),
        );
        assert!(error.contains("no current edge"), "{error}");
    }

    #[test]
    fn compaction_keeps_the_state_and_drops_the_history() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let mut overlay = Overlay::open(&session, &path).unwrap();
        let mut again = add_field(8, "h3");
        if let Record::Add { confidence, .. } = &mut again {
            *confidence = Confidence::Low;
        }
        overlay
            .record(
                &session,
                vec![
                    add_field(8, "h3"),
                    again,
                    add_field(8, "h1"),
                    Record::Retract {
                        edge: field_key("h1"),
                        reason: "never stored".into(),
                    },
                    Record::Verify {
                        edge: field_key("h3"),
                        tool: "reviewer".into(),
                        verdict: Verdict::Inconclusive,
                        at: Some("2026-10-05".into()),
                    },
                ],
            )
            .unwrap();
        assert!(
            overlay.compact().is_err(),
            "compaction refuses while records are pending"
        );
        assert_eq!(overlay.save().unwrap(), 5);
        overlay.compact().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 3, "header, one add, one verify");
        let reopened = Overlay::open(&session, &path).unwrap();
        assert_eq!(state(&reopened), state(&overlay));
        assert_eq!(
            reopened.edges().next().unwrap().confidence,
            Confidence::Low,
            "the later add won"
        );
    }

    #[test]
    fn an_overlay_for_another_build_refuses_to_load() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let mut overlay = Overlay::open(&session, &path).unwrap();
        overlay.record(&session, vec![add_field(8, "h3")]).unwrap();
        overlay.save().unwrap();

        let mut rebuilt: ProgramFacts = facts_with_overlay_sites();
        rebuilt.modules[1].content_sha256 = Some("cc".into());
        let rebuilt = Session::new(rebuilt, Vec::new());
        let error = open_error(&rebuilt, &path);
        assert!(error.contains(":1:"), "{error}");
        assert!(error.contains("different build"), "{error}");
        assert!(error.contains(&fingerprint(&session).unwrap()), "{error}");
        assert!(error.contains(&fingerprint(&rebuilt).unwrap()), "{error}");
    }

    #[test]
    fn an_unhashed_module_makes_the_overlay_unbindable() {
        let mut unhashed = facts_with_overlay_sites();
        unhashed.modules[1].content_sha256 = None;
        let session = Session::new(unhashed, Vec::new());
        let error = message(fingerprint(&session).unwrap_err());
        assert!(error.contains("mod_b"), "{error}");
        assert!(Overlay::empty(&session, None).is_err());

        let scratch = tempfile::tempdir().unwrap();
        let error = open_error(&session, &overlay_path(&scratch));
        assert!(error.contains("mod_b"), "{error}");
    }

    #[test]
    fn a_bad_line_names_its_line_number() {
        let scratch = tempfile::tempdir().unwrap();
        let path = overlay_path(&scratch);
        let session = session();
        let good = serde_json::to_string(&add_field(8, "h3")).unwrap();
        let shown = path.display().to_string();

        std::fs::write(&path, format!("{}{good}\n{{not json\n", header(&session))).unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:3:")), "{error}");

        std::fs::write(&path, format!("{good}\n")).unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:1:")), "{error}");

        std::fs::write(&path, "").unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:1:")), "{error}");

        std::fs::write(&path, "{\"v\":2,\"fingerprint\":\"x\"}\n").unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:1:")), "{error}");

        std::fs::write(
            &path,
            format!("{}{{\"op\":\"annotate\"}}\n", header(&session)),
        )
        .unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:2:")), "{error}");
        assert!(error.contains("unknown op \"annotate\""), "{error}");

        let misspelled = good.replace("\"provenance\"", "\"provenence\"");
        std::fs::write(&path, format!("{}{misspelled}\n", header(&session))).unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:2:")), "{error}");
        assert!(error.contains("provenence"), "{error}");

        let ungrounded = serde_json::to_string(&add_field(16, "h3")).unwrap();
        std::fs::write(&path, format!("{}{ungrounded}\n", header(&session))).unwrap();
        let error = open_error(&session, &path);
        assert!(error.contains(&format!("{shown}:2:")), "{error}");
        assert!(error.contains("ops@16"), "{error}");
    }

    #[test]
    fn a_bounded_site_rejects_an_agent_edge() {
        let session = session();
        let site = site_in(&session, "bounded");
        let error = record_error(
            &session,
            vec![add(None, Some(site), TargetSpec::Symbol("h3".into()))],
        );
        assert!(
            error.contains("the site has an LLVM bound; an agent edge may not widen it"),
            "{error}"
        );
    }

    #[test]
    fn a_direct_site_rejects_an_agent_edge() {
        let session = session();
        let site = site_in(&session, "init");
        let error = record_error(
            &session,
            vec![add(None, Some(site), TargetSpec::Symbol("h1".into()))],
        );
        assert!(error.contains("not an indirect call site"), "{error}");
    }

    #[test]
    fn an_add_names_exactly_one_of_a_field_and_a_site() {
        let session = session();
        let site = site_in(&session, "call_plain");
        for record in [
            add(None, None, TargetSpec::Symbol("h1".into())),
            add(Some(ops(8)), Some(site), TargetSpec::Symbol("h1".into())),
        ] {
            let error = record_error(&session, vec![record]);
            assert!(error.contains("exactly one"), "{error}");
        }
    }

    #[test]
    fn a_field_no_site_dispatches_through_is_rejected() {
        let session = session();
        let error = record_error(&session, vec![add_field(16, "h3")]);
        assert!(
            error.contains("no unresolved indirect call site dispatches through ops@16"),
            "{error}"
        );
    }

    #[test]
    fn an_ambiguous_target_lists_its_modules() {
        let session = session();
        let error = record_error(&session, vec![add_field(8, "helper")]);
        assert!(
            error.contains("mod_a") && error.contains("mod_b"),
            "{error}"
        );

        let mut overlay = Overlay::empty(&session, None).unwrap();
        overlay
            .record(
                &session,
                vec![add(
                    Some(ops(8)),
                    None,
                    TargetSpec::Id(id("mod_b", "helper")),
                )],
            )
            .unwrap();
        assert_eq!(
            overlay.edges().next().unwrap().key,
            EdgeKey::Field {
                via_field: ops(8),
                to: id("mod_b", "helper")
            },
            "a full id names one of them"
        );

        let error = record_error(
            &session,
            vec![add(Some(ops(8)), None, TargetSpec::Id(id("mod_b", "h3")))],
        );
        assert!(error.contains("h3"), "an id must exist: {error}");
    }

    #[test]
    fn a_batch_with_one_bad_record_applies_none() {
        let session = session();
        let mut overlay = Overlay::empty(&session, None).unwrap();
        let error = message(
            overlay
                .record(&session, vec![add_field(8, "h3"), add_field(16, "h3")])
                .unwrap_err(),
        );
        assert!(error.starts_with("record 2: "), "{error}");
        assert_eq!(overlay.edges().count(), 0);
        assert_eq!(overlay.pending(), 0);
    }

    #[test]
    fn a_source_other_than_agent_is_rejected() {
        let session = session();
        let mut record = add_field(8, "h3");
        if let Record::Add { source, .. } = &mut record {
            *source = "human".into();
        }
        let error = record_error(&session, vec![record]);
        assert!(error.contains("agent"), "{error}");

        let mut parsed: Record = r#"{"op":"add","via_field":{"record":"ops","offset":8},"to":"h3","confidence":"high","provenance":["p"]}"#
            .parse()
            .unwrap();
        assert!(matches!(&parsed, Record::Add { source, .. } if source == "agent"));
        if let Record::Add { provenance, .. } = &mut parsed {
            provenance.clear();
        }
        let error = record_error(&session, vec![parsed]);
        assert!(error.contains("provenance"), "{error}");
    }

    #[test]
    fn coverage_counts_sites_an_unrefuted_edge_attaches_to() {
        let session = session();
        let mut overlay = Overlay::empty(&session, None).unwrap();
        let plain = site_in(&session, "call_plain");
        overlay
            .record(
                &session,
                vec![
                    add_field(8, "h3"),
                    add(None, Some(plain.clone()), TargetSpec::Symbol("h1".into())),
                ],
            )
            .unwrap();
        let summary = overlay.summary(&session);
        assert_eq!(summary.coverage.unresolved_sites, 2);
        assert_eq!(summary.coverage.covered_sites, 2);
        assert!(summary.edges.iter().all(|edge| edge.sites == 1));

        overlay
            .record(
                &session,
                vec![Record::Verify {
                    edge: EdgeKey::Site {
                        site: plain,
                        to: id("mod_a", "h1"),
                    },
                    tool: "reviewer".into(),
                    verdict: Verdict::Refuted,
                    at: None,
                }],
            )
            .unwrap();
        let summary = overlay.summary(&session);
        assert_eq!(summary.coverage.covered_sites, 1);
        assert_eq!(summary.pending, 3);
    }

    #[test]
    fn the_default_overlay_sits_beside_its_catalog() {
        assert_eq!(
            default_overlay_path(Path::new("build/catalog.json")),
            Path::new("build/catalog.overlay.jsonl")
        );
    }
}
