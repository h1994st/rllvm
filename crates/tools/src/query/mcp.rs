//! MCP stdio server: the nine source-level queries as JSON-RPC 2.0 tools,
//! newline-delimited over stdin/stdout, plus four that decide which catalogs
//! they run against.
//!
//! The catalog is chosen by the client, not by the command line. A server
//! pinned to one catalog at startup could not switch programs, compare two
//! builds, or work in a repository where no catalog exists yet -- all of
//! which an interactive session does. [`Registry`] holds what has been
//! loaded for the life of the process, so a catalog's analysis is paid once
//! and every query after it answers from memory. `--catalog` survives as a
//! preload for a client that always analyses the same program.
//!
//! Dual-era, exactly as the specification permits. A request whose
//! `params._meta["io.modelcontextprotocol/protocolVersion"]` is present is
//! modern (`MODERN`): served statelessly, checked against `MODERN`, and
//! rejected with `-32022` (naming what is supported) when it does not
//! match. An `initialize` request carries no such `_meta` block and selects
//! legacy (`LEGACY`) instead. Per the 2025-06-18 lifecycle spec, an
//! unsupported requested version there gets a *counter-offer* -- a
//! successful `InitializeResult` naming `LEGACY` -- not a protocol error: a
//! legacy client has no fall-forward mechanism, so an error would be a dead
//! end the counter-offer exists to avoid. Both version strings are fixed
//! literals: accepting and echoing whatever a client sends would claim
//! conformance this server has never been checked against.
//!
//! No connection state is kept for this beyond the request in hand. A
//! legacy client never sends the modern `_meta` block on any message, so
//! classifying each request independently already gives it legacy framing
//! for the life of the process; a modern client is stateless by design and
//! never needs a handshake at all (see
//! `a_modern_request_is_served_without_a_handshake` in `tests/query.rs`).
//!
//! stdout carries protocol frames only, the same rule the compiler wrappers
//! follow for their own stdout: every diagnostic belongs on stderr, and
//! `serve` itself never writes anything but one JSON-RPC response line per
//! request that expects one.
//!
//! Tool names (`query_tool`, `query_from_call`) are `snake_case`, matching
//! `Query`'s own serde `kind` tag, so a result envelope's `"kind":
//! "indirect_targets"` feeds straight back into `tools/call`'s `name`. CLI
//! subcommands (`cli::QueryCommand`) are `kebab-case` instead, clap's
//! convention. The two spellings diverge only in these literals; nothing
//! ties them together.

use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use crate::error::Error;

use super::{Direction, Query, Session, analysis_of, open, open_catalog, run};

/// The modern protocol revision this server has been checked against.
const MODERN: &str = "2026-07-28";
/// The legacy protocol revision an `initialize` handshake may select.
const LEGACY: &str = "2025-06-18";

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

/// The catalogs one MCP session has loaded, keyed by the canonical path they
/// came from: the catalog JSON for `load_catalog`, the artifact itself for
/// `inventory`. Loading the same path twice replaces its entry, which is
/// what an agent wants after a rebuild.
///
/// Held by the caller across the whole `serve` loop, so the analysis a
/// catalog costs is paid once however many queries follow -- and so an agent
/// can point the server at a second program without restarting it.
#[derive(Default)]
pub struct Registry {
    sessions: BTreeMap<PathBuf, Session>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Reads the catalog JSON at `path` and makes it queryable. Returns the
    /// same summary the `load_catalog` tool answers with: what the catalog
    /// claims, and what actually parsed.
    pub fn load(&mut self, path: &Path) -> Result<Value, Error> {
        let key = path.canonicalize()?;
        let session = open(&key)?;
        Ok(self.insert(key, session))
    }

    /// Inventories a captured binary, archive or `.bc` and loads the catalog
    /// that produces. The catalog is never written to disk: an agent asking
    /// about an artifact wants an answer, not a file it has to name.
    fn inventory(&mut self, artifact: &Path, bitcode_root: &Path) -> Result<Value, Error> {
        let key = artifact.canonicalize()?;
        let catalog = crate::catalog::inventory(&key, bitcode_root, None)?;
        let directory = key.parent().unwrap_or(Path::new(".")).to_path_buf();
        let session = open_catalog(catalog, &directory)?;
        Ok(self.insert(key, session))
    }

    /// A registry holding sessions under names of the test's choosing, since
    /// the fixtures here are built in memory and never came from a path.
    #[cfg(test)]
    pub(crate) fn with_sessions(sessions: Vec<(&str, Session)>) -> Registry {
        Registry {
            sessions: sessions
                .into_iter()
                .map(|(key, session)| (PathBuf::from(key), session))
                .collect(),
        }
    }

    fn insert(&mut self, key: PathBuf, session: Session) -> Value {
        let summary = catalog_summary(&key, &session);
        self.sessions.insert(key, session);
        summary
    }

    fn unload(&mut self, requested: &str) -> Result<Value, String> {
        match self.key_for(requested) {
            Some(key) => {
                self.sessions.remove(&key);
                Ok(json!({ "unloaded": key.display().to_string(), "loaded": self.loaded() }))
            }
            None => Err(self.not_loaded(requested)),
        }
    }

    fn list(&self) -> Value {
        json!({
            "catalogs": self
                .sessions
                .iter()
                .map(|(key, session)| catalog_summary(key, session))
                .collect::<Vec<_>>()
        })
    }

    /// The session a call names, or the only one loaded.
    ///
    /// `catalog` is optional exactly while one catalog is loaded, which keeps
    /// the common single-program session free of ceremony. With none loaded
    /// or several, the message names what to do rather than guessing: picking
    /// one of several would answer confidently about the wrong program.
    fn session(&self, requested: Option<&str>) -> Result<&Session, String> {
        if let Some(requested) = requested {
            return match self
                .key_for(requested)
                .and_then(|key| self.sessions.get(&key))
            {
                Some(session) => Ok(session),
                None => Err(self.not_loaded(requested)),
            };
        }
        let mut loaded = self.sessions.values();
        match (loaded.next(), loaded.next()) {
            (Some(only), None) => Ok(only),
            (None, _) => Err(
                "no catalog is loaded: call `load_catalog` with a catalog JSON, or `inventory` \
                 with a captured binary, archive or .bc file"
                    .to_string(),
            ),
            _ => Err(format!(
                "several catalogs are loaded; name one in `catalog`: {}",
                self.loaded().join(", ")
            )),
        }
    }

    /// Accepts the key as reported and, failing that, whatever the path
    /// canonicalizes to now: an agent that passes the path it typed rather
    /// than the one it was handed back should still be understood.
    fn key_for(&self, requested: &str) -> Option<PathBuf> {
        let as_given = PathBuf::from(requested);
        if self.sessions.contains_key(&as_given) {
            return Some(as_given);
        }
        let canonical = as_given.canonicalize().ok()?;
        self.sessions.contains_key(&canonical).then_some(canonical)
    }

    fn loaded(&self) -> Vec<String> {
        self.sessions
            .keys()
            .map(|key| key.display().to_string())
            .collect()
    }

    fn not_loaded(&self, requested: &str) -> String {
        format!(
            "no catalog loaded as `{requested}`; loaded: {}",
            match self.loaded() {
                names if names.is_empty() => "none".to_string(),
                names => names.join(", "),
            }
        )
    }
}

/// What a load or a listing reports about one catalog: the scope it claims
/// and what actually parsed. The same two blocks every query answer carries,
/// so an agent sees the analysis before it asks its first question.
fn catalog_summary(key: &Path, session: &Session) -> Value {
    json!({
        "catalog": key.display().to_string(),
        "scope": session.scope(),
        "analysis": analysis_of(session.modules()),
    })
}

/// Which era a request was classified into, and therefore which result
/// fields its response may carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Era {
    /// `resultType: "complete"` on every result, plus `ttlMs`/`cacheScope`
    /// on results that extend `CacheableResult`.
    Modern,
    /// None of the modern result fields.
    Legacy,
}

/// What a method handler produced, before era-specific fields are merged in.
enum Outcome {
    /// The result payload, and whether it extends `CacheableResult` (and so
    /// gains `ttlMs`/`cacheScope` in the modern era).
    Result(Value, bool),
    /// A JSON-RPC protocol error: malformed request, unknown method, or an
    /// unsupported protocol version. Not for a query that merely found
    /// nothing, or a `tools/call` whose arguments did not resolve to a
    /// query -- those are reported inside a `CallToolResult` with
    /// `isError: true` instead, so the client can display them.
    Error(i64, String, Option<Value>),
}

/// Serve JSON-RPC 2.0 requests, one per line, until `input` reaches EOF.
///
/// `registry` may start empty: a client loads catalogs through the
/// `load_catalog` and `inventory` tools and keeps them for the life of the
/// process, so each catalog's analysis is paid once however many queries
/// follow. A caller that already knows the catalog can preload it, and then
/// the first `tools/call` pays nothing at all.
pub fn serve(
    registry: &mut Registry,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<(), Error> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_line(registry, trimmed) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

/// Handle one request line. Returns `None` only for a notification: a
/// request object carrying a `method` and no `id`. Everything else is
/// answered, including a value that is not a request object at all.
fn handle_line(registry: &mut Registry, line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                -32700,
                &format!("Parse error: {error}"),
                None,
            ));
        }
    };

    // A valid-JSON value that is not an object, and an object with no
    // string `method`, are both Invalid Request (-32600), not an unknown
    // method and not silence. JSON-RPC 2.0 requires `id: null` when the id
    // cannot be determined, which is what a non-object has.
    let Some(object) = request.as_object() else {
        return Some(error_response(
            Value::Null,
            -32600,
            "Invalid Request: expected a JSON-RPC 2.0 request object",
            None,
        ));
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            -32600,
            "Invalid Request: `method` must be a string",
            None,
        ));
    };
    // A request with a method and no `id` is a notification, which the
    // JSON-RPC 2.0 contract never answers -- `notifications/initialized` is
    // handled this way, with no method-specific case needed.
    object.get("id")?;
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));

    let era = match classify_era(&params) {
        Ok(era) => era,
        Err((code, message, data)) => return Some(error_response(id, code, &message, data)),
    };

    let outcome = match (method, era) {
        ("initialize", Era::Legacy) => Outcome::Result(legacy_initialize_result(), false),
        ("ping", _) => Outcome::Result(json!({}), false),
        ("server/discover", _) => Outcome::Result(discover_result(), true),
        ("tools/list", _) => Outcome::Result(tools_list_result(), true),
        ("tools/call", _) => tool_call_outcome(registry, &params),
        _ => Outcome::Error(-32601, format!("unknown method: {method}"), None),
    };

    Some(match outcome {
        Outcome::Error(code, message, data) => error_response(id, code, &message, data),
        Outcome::Result(mut result, cacheable) => {
            // `Value`'s `IndexMut` panics on a non-object `Value`; no
            // handler returns one today, but library code here must not
            // panic if that ever changes, so this goes through
            // `as_object_mut` instead of indexed assignment.
            if era == Era::Modern
                && let Some(map) = result.as_object_mut()
            {
                map.insert("resultType".into(), json!("complete"));
                if cacheable {
                    map.insert("ttlMs".into(), json!(60_000));
                    map.insert("cacheScope".into(), json!("session"));
                }
            }
            success_response(id, result)
        }
    })
}

/// Classifies a request into its era, and validates the version-selection
/// fields a modern request requires: `clientCapabilities` alongside the
/// modern `_meta` block, and a `protocolVersion` that matches `MODERN`.
/// Returns the JSON-RPC error to send when either check fails.
///
/// A legacy `initialize`'s requested version is not validated here: the
/// 2025-06-18 lifecycle spec requires a counter-offer, not an error, when
/// the requested version is unsupported ("If the server supports the
/// requested protocol version, it MUST respond with the same version.
/// Otherwise, the server MUST respond with another protocol version it
/// supports."). `legacy_initialize_result` always names `LEGACY`, which
/// satisfies both branches of that sentence at once, so `initialize` always
/// selects `Era::Legacy` regardless of what was requested -- a legacy
/// client has no fall-forward mechanism, so an error here would be exactly
/// the dead end the counter-offer exists to avoid.
fn classify_era(params: &Value) -> Result<Era, (i64, String, Option<Value>)> {
    let meta = params.get("_meta");
    let modern_version = meta
        .and_then(|meta| meta.get(META_PROTOCOL_VERSION))
        .and_then(Value::as_str);

    if let Some(version) = modern_version {
        let has_capabilities = meta
            .and_then(|meta| meta.get(META_CLIENT_CAPABILITIES))
            .is_some();
        if !has_capabilities {
            return Err((
                -32602,
                format!("modern requests must carry params._meta[\"{META_CLIENT_CAPABILITIES}\"]"),
                None,
            ));
        }
        if version != MODERN {
            return Err(unsupported_version(version));
        }
        return Ok(Era::Modern);
    }

    Ok(Era::Legacy)
}

fn unsupported_version(requested: &str) -> (i64, String, Option<Value>) {
    (
        -32022,
        format!("unsupported protocol version: {requested}"),
        Some(json!({ "supported": [MODERN, LEGACY] })),
    )
}

fn legacy_initialize_result() -> Value {
    json!({
        "protocolVersion": LEGACY,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "rllvm-query", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn discover_result() -> Value {
    json!({
        "supportedVersions": [LEGACY, MODERN],
        "capabilities": { "tools": {} }
    })
}

fn tools_list_result() -> Value {
    let tools: Vec<Value> = Management::all()
        .into_iter()
        .map(Management::tool)
        .chain(sample_queries().iter().map(tool_for))
        .collect();
    json!({ "tools": tools })
}

/// Declares the catalog-management surface once, under the same rule
/// `query_tool_surface!` follows: `all`, `name`, `tool` and `from_name` are
/// generated from one list, so a tool cannot be listed under a name
/// `tools/call` does not recognize, nor recognized under one `tools/list`
/// never reports.
macro_rules! management_tool_surface {
    ($($variant:ident => $name:literal, $description:literal, $schema:expr;)+) => {
        /// A tool that decides which catalogs are loaded, as opposed to the
        /// nine that ask a question of one.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Management { $($variant),+ }

        impl Management {
            fn all() -> Vec<Management> {
                vec![$(Management::$variant),+]
            }

            fn name(self) -> &'static str {
                match self { $(Management::$variant => $name,)+ }
            }

            fn from_name(name: &str) -> Option<Management> {
                match name {
                    $($name => Some(Management::$variant),)+
                    _ => None,
                }
            }

            fn tool(self) -> Value {
                // Through `name`, as `query_tool` goes through `tool_name_of`:
                // the listing and the name the dispatcher matches are one
                // string, not two literals that happen to agree today.
                let name = self.name();
                match self {
                    $(Management::$variant => json!({
                        "name": name,
                        "description": $description,
                        "inputSchema": $schema,
                    }),)+
                }
            }
        }
    };
}

management_tool_surface! {
    Load => "load_catalog",
        "Load a catalog written by `rllvm-get-bc --output-dir` or `rllvm-compdb generate`, and keep it queryable for the rest of this session. Answers with what the catalog claims and what actually parsed.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the catalog JSON" }
            },
            "required": ["path"]
        });
    Inventory => "inventory",
        "Inventory a binary, archive or .bc file rllvm captured bitcode for, and load the catalog it yields. Nothing is written to disk. Use this when no catalog JSON exists yet.",
        json!({
            "type": "object",
            "properties": {
                "artifact": {
                    "type": "string",
                    "description": "Binary, archive or .bc file with recorded bitcode paths"
                },
                "bitcode_root": {
                    "type": "string",
                    "description": "Directory that relative recorded module paths resolve against (default: the working directory)"
                }
            },
            "required": ["artifact"]
        });
    List => "list_catalogs",
        "Every catalog loaded right now, each with what the catalog claims and what actually parsed.",
        json!({ "type": "object", "properties": {} });
    Unload => "unload_catalog",
        "Drop a loaded catalog and release the facts extracted from it.",
        json!({
            "type": "object",
            "properties": {
                "catalog": {
                    "type": "string",
                    "description": "Catalog to drop, named as `load_catalog` reported it"
                }
            },
            "required": ["catalog"]
        });
}

/// Runs one catalog-management call. Every failure here is a message the
/// client can act on -- a path that does not exist, a catalog that was never
/// loaded -- so all of them come back as tool errors, never protocol errors.
fn management_outcome(
    registry: &mut Registry,
    tool: Management,
    arguments: &Value,
) -> Result<Value, String> {
    match tool {
        Management::Load => registry
            .load(Path::new(&string_argument(arguments, "path")?))
            .map_err(|error| error.to_string()),
        Management::Inventory => {
            let artifact = string_argument(arguments, "artifact")?;
            let root = arguments
                .get("bitcode_root")
                .and_then(Value::as_str)
                .unwrap_or(".");
            registry
                .inventory(Path::new(&artifact), Path::new(root))
                .map_err(|error| error.to_string())
        }
        Management::List => Ok(registry.list()),
        Management::Unload => registry.unload(&string_argument(arguments, "catalog")?),
    }
}

/// One string argument, or the message a client sees when it is missing.
fn string_argument(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing or non-string argument `{key}`"))
}

/// Declares the tool surface once: for each `Query` variant, the pattern
/// that matches it, a sample instance, and the tool name.
///
/// This is what keeps the surface from going stale. The generated
/// `tool_name_of` is a match over `Query` with no wildcard arm, so a variant
/// missing from the list below fails to compile -- and because
/// `sample_queries` is generated from the same list, being declared here is
/// what puts a tool into `tools/list` at all. A hand-written `[Query; 9]`
/// could not do that: its length has no relationship to the variant count,
/// so a tenth query would compile, go unlisted, and be permanently
/// uncallable over MCP while working fine on the command line.
///
/// The sample field values are placeholders that only select a match arm in
/// `tool_for`; they never reach a session.
macro_rules! query_tool_surface {
    ($($pattern:pat => $sample:expr, $name:literal;)+) => {
        /// One instance per `Query` variant, so `tools/list` reports them all.
        fn sample_queries() -> Vec<Query> {
            vec![$($sample),+]
        }

        /// The MCP tool name for one `Query` variant. Exhaustive over
        /// `Query` with no wildcard arm.
        fn tool_name_of(query: &Query) -> &'static str {
            match query {
                $($pattern => $name,)+
            }
        }
    };
}

query_tool_surface! {
    Query::Defs { .. } => Query::Defs { name: String::new() }, "defs";
    Query::At { .. } => Query::At { file: String::new(), line: 0 }, "at";
    Query::Callers { .. } => Query::Callers { name: String::new() }, "callers";
    Query::Callees { .. } => Query::Callees { name: String::new() }, "callees";
    Query::Uses { .. } => Query::Uses { name: String::new() }, "uses";
    Query::Reach { .. } => Query::Reach { from: String::new(), to: String::new() }, "reach";
    Query::Closure { .. } => Query::Closure { name: String::new(), direction: Direction::In }, "closure";
    Query::Externals => Query::Externals, "externals";
    Query::IndirectTargets { .. } => Query::IndirectTargets { at: String::new(), heuristics: false }, "indirect_targets";
}

/// The `name` argument four of the nine tools take, identically. Spelled
/// once: four copies of one schema drift apart, and a client reads the
/// drifted one as a real difference between the tools.
fn symbol_property() -> Value {
    json!({
        "type": "string",
        "description": "Symbol to look up. Accepts a mangled symbol (`_Z5twiceIiET_S0_`), a full demangled reading (`int twice<int>(int)`), or a bare identifier (`twice`), which matches every function whose reading contains it. The answer's `resolution` block says which applied."
    })
}

/// A tool whose only argument is that symbol name.
fn symbol_tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": { "name": symbol_property() },
            "required": ["name"]
        }
    })
}

/// Which loaded catalog a query runs against. Added to all nine schemas at
/// one point below rather than written into each: nine copies of an optional
/// argument drift, and a client reads the drift as a real difference.
fn catalog_property() -> Value {
    json!({
        "type": "string",
        "description": "Loaded catalog to query, named as `load_catalog` reported it. Optional while exactly one catalog is loaded."
    })
}

/// The MCP tool definition for one `Query` variant: name, description, and
/// JSON input schema, plus the shared optional `catalog` argument.
fn tool_for(query: &Query) -> Value {
    let mut tool = query_tool(query);
    if let Some(properties) = tool
        .get_mut("inputSchema")
        .and_then(|schema| schema.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        properties.insert("catalog".into(), catalog_property());
    }
    tool
}

/// The query half of a tool definition. Exhaustive over `Query` with no
/// wildcard arm, and the name is taken from `tool_name_of` so the listing
/// and the name a client calls back cannot be spelled differently.
fn query_tool(query: &Query) -> Value {
    let name = tool_name_of(query);
    match query {
        Query::Defs { .. } => symbol_tool(
            name,
            "Every definition of the symbol, with module and configuration.",
        ),
        Query::At { .. } => json!({
            "name": name,
            "description": "Functions with at least one instruction mapped to file:line, and the call sites recorded there.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Source file, as recorded in debug info" },
                    "line": { "type": "integer", "minimum": 1, "description": "One-based source line" }
                },
                "required": ["file", "line"]
            }
        }),
        Query::Callers { .. } => symbol_tool(
            name,
            "Functions containing a call to the target, each with its call sites.",
        ),
        Query::Callees { .. } => symbol_tool(
            name,
            "Outgoing call sites of the target, classified. Includes unresolved indirect sites.",
        ),
        Query::Uses { .. } => symbol_tool(
            name,
            "Non-call uses: how and where the function's address is taken.",
        ),
        Query::Reach { .. } => json!({
            "name": name,
            "description": "One supporting path from `from` to `to`, or its explicit absence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Symbol to start from; same spellings as `defs` accepts" },
                    "to": { "type": "string", "description": "Symbol to reach; same spellings as `defs` accepts" }
                },
                "required": ["from", "to"]
            }
        }),
        Query::Closure { .. } => json!({
            "name": name,
            "description": "The set that can reach the target (in) or that it can reach (out).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": symbol_property(),
                    "direction": {
                        "type": "string",
                        "enum": ["in", "out"],
                        "description": "`in` for functions that reach it, `out` for functions it reaches"
                    }
                },
                "required": ["name", "direction"]
            }
        }),
        Query::Externals => json!({
            "name": name,
            "description": "Unbound symbols: the captured program's boundary.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        Query::IndirectTargets { .. } => json!({
            "name": name,
            "description": "`!callees` at a call site, when CVP produced it; otherwise unresolved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "at": { "type": "string", "description": "`file:line` location, e.g. `t.c:4`" },
                    "heuristics": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include the heuristic address-taken inventory"
                    }
                },
                "required": ["at"]
            }
        }),
    }
}

/// Runs a `tools/call`, management tool or query.
///
/// Everything a client can get wrong -- an unknown tool, missing arguments,
/// a catalog that will not load, a query naming no loaded catalog, a
/// location that does not parse -- comes back as a `CallToolResult` with
/// `isError: true` rather than a protocol error, so the client can read what
/// was wrong with the call it made and try again.
fn tool_call_outcome(registry: &mut Registry, params: &Value) -> Outcome {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Outcome::Error(
            -32602,
            "tools/call requires a string `name`".to_string(),
            None,
        );
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    if let Some(tool) = Management::from_name(name) {
        return match management_outcome(registry, tool, &arguments) {
            Ok(payload) => Outcome::Result(call_tool_result(false, &payload.to_string()), false),
            Err(message) => Outcome::Result(call_tool_result(true, &message), false),
        };
    }

    let query = match query_from_call(name, &arguments) {
        Ok(query) => query,
        Err(message) => return Outcome::Result(call_tool_result(true, &message), false),
    };
    let session = match registry.session(arguments.get("catalog").and_then(Value::as_str)) {
        Ok(session) => session,
        Err(message) => return Outcome::Result(call_tool_result(true, &message), false),
    };

    // A query the session could not interpret -- an `indirect_targets`
    // location that does not parse -- is a tool error the client can
    // display, not a protocol error, and never an empty `results` list that
    // would read as a valid answer.
    let result = match run(session, &query) {
        Ok(result) => result,
        Err(error) => return Outcome::Result(call_tool_result(true, &error.to_string()), false),
    };
    let payload = serde_json::to_value(&result)
        .unwrap_or_else(|error| json!({ "serialization_error": error.to_string() }));
    Outcome::Result(call_tool_result(false, &payload.to_string()), false)
}

fn call_tool_result(is_error: bool, text: &str) -> Value {
    json!({
        "isError": is_error,
        "content": [{ "type": "text", "text": text }]
    })
}

/// Resolves one tool call's arguments into the `Query` it names. The
/// counterpart to `tool_for`: `tool_for` is exhaustive over `Query` and
/// enforced by the compiler, while this is exhaustive over the tool-name
/// strings those variants chose, checked by `tools/list`'s own answer
/// containing every name this recognizes.
fn query_from_call(name: &str, arguments: &Value) -> Result<Query, String> {
    let string_field = |key: &str| string_argument(arguments, key);

    match name {
        "defs" => Ok(Query::Defs {
            name: string_field("name")?,
        }),
        "at" => {
            let file = string_field("file")?;
            let line = arguments
                .get("line")
                .and_then(Value::as_u64)
                .and_then(|line| u32::try_from(line).ok())
                .ok_or_else(|| "missing or invalid argument `line`".to_string())?;
            Ok(Query::At { file, line })
        }
        "callers" => Ok(Query::Callers {
            name: string_field("name")?,
        }),
        "callees" => Ok(Query::Callees {
            name: string_field("name")?,
        }),
        "uses" => Ok(Query::Uses {
            name: string_field("name")?,
        }),
        "reach" => Ok(Query::Reach {
            from: string_field("from")?,
            to: string_field("to")?,
        }),
        "closure" => {
            let name = string_field("name")?;
            let direction = match string_field("direction")?.as_str() {
                "in" => Direction::In,
                "out" => Direction::Out,
                other => {
                    return Err(format!(
                        "unknown direction `{other}`, expected `in` or `out`"
                    ));
                }
            };
            Ok(Query::Closure { name, direction })
        }
        "externals" => Ok(Query::Externals),
        "indirect_targets" => Ok(Query::IndirectTargets {
            at: string_field("at")?,
            heuristics: arguments
                .get("heuristics")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::testing::session_from;

    fn one_catalog() -> Registry {
        Registry::with_sessions(vec![("first", session_from(&[("a", "b")]))])
    }

    /// Sample arguments for one tool, keyed by the same `Query` variant the
    /// tool was listed from. Exhaustive over `Query` with no wildcard arm,
    /// like `tool_name_of` itself, so a new variant fails to compile here
    /// too.
    fn sample_arguments(query: &Query) -> Value {
        match query {
            Query::Defs { .. } => json!({ "name": "a" }),
            Query::At { .. } => json!({ "file": "t.c", "line": 1 }),
            Query::Callers { .. } => json!({ "name": "a" }),
            Query::Callees { .. } => json!({ "name": "a" }),
            Query::Uses { .. } => json!({ "name": "a" }),
            Query::Reach { .. } => json!({ "from": "a", "to": "b" }),
            Query::Closure { .. } => json!({ "name": "a", "direction": "in" }),
            Query::Externals => json!({}),
            Query::IndirectTargets { .. } => json!({ "at": "t.c:4" }),
        }
    }

    fn listed_tool_names() -> Vec<String> {
        tools_list_result()["tools"]
            .as_array()
            .expect("tools/list must report an array")
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Calls one tool and returns its `CallToolResult`, which is where every
    /// client-visible failure lands.
    fn call(registry: &mut Registry, name: &str, arguments: Value) -> Value {
        let params = json!({ "name": name, "arguments": arguments });
        let Outcome::Result(result, _) = tool_call_outcome(registry, &params) else {
            panic!("`{name}` must produce a CallToolResult, not a protocol error");
        };
        result
    }

    fn call_text(registry: &mut Registry, name: &str, arguments: Value) -> String {
        call(registry, name, arguments)["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// The guard the fixed-length `[Query; 9]` could not give: every variant
    /// declared in `query_tool_surface!` -- and the macro's `tool_name_of`
    /// match makes that every variant, or the crate does not compile --
    /// reaches `tools/list` under its own name and comes back through
    /// `query_from_call` as the same variant.
    #[test]
    fn every_query_variant_is_listed_and_resolves_through_a_call() {
        let listed = listed_tool_names();
        for query in sample_queries() {
            let name = tool_name_of(&query);
            assert!(
                listed.iter().any(|listed| listed == name),
                "tools/list does not report `{name}`: {listed:?}"
            );
            let resolved = query_from_call(name, &sample_arguments(&query))
                .unwrap_or_else(|error| panic!("`{name}` did not resolve: {error}"));
            assert_eq!(
                tool_name_of(&resolved),
                name,
                "`{name}` resolved to a different query variant"
            );
        }
    }

    /// The same guard for the management half, and the accounting that keeps
    /// the two halves exhaustive between them: a listed tool that is neither
    /// a `Management` variant nor a `Query` variant is a tool nothing can
    /// dispatch.
    #[test]
    fn every_listed_tool_is_a_management_tool_or_a_query() {
        let listed = listed_tool_names();
        for tool in Management::all() {
            assert!(
                listed.iter().any(|listed| listed == tool.name()),
                "tools/list does not report `{}`: {listed:?}",
                tool.name()
            );
            assert_eq!(
                Management::from_name(tool.name()),
                Some(tool),
                "`{}` resolved to a different management tool",
                tool.name()
            );
        }
        assert_eq!(
            listed.len(),
            Management::all().len() + sample_queries().len(),
            "every listed tool must come from exactly one variant of one of the two surfaces"
        );
    }

    /// A tool name is one spelling on both sides: listed once, and the same
    /// literal the dispatcher matches. Covers collisions across the two
    /// surfaces too -- a query named `inventory` would be unreachable,
    /// because management names are matched first.
    #[test]
    fn no_two_tools_share_a_name() {
        let mut listed = listed_tool_names();
        listed.sort();
        let count = listed.len();
        listed.dedup();
        assert_eq!(listed.len(), count, "duplicate tool name in tools/list");
    }

    /// Every query tool takes the same optional `catalog` argument. It is
    /// added at one point in `tool_for`, and this is what says it reached
    /// all nine.
    #[test]
    fn every_query_tool_accepts_a_catalog_argument() {
        for query in sample_queries() {
            let tool = tool_for(&query);
            assert!(
                tool["inputSchema"]["properties"]["catalog"].is_object(),
                "`{}` does not accept a catalog: {tool}",
                tool_name_of(&query)
            );
            assert!(
                !tool["inputSchema"]["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|key| key == "catalog")),
                "`catalog` must stay optional on `{}`",
                tool_name_of(&query)
            );
        }
    }

    /// With nothing loaded a query cannot be answered, and saying so is not
    /// the same as answering `results: []` -- which is what a server that
    /// treated "no catalog" as "no facts" would return.
    #[test]
    fn a_query_with_no_catalog_loaded_says_how_to_load_one() {
        let mut registry = Registry::new();
        let result = call(&mut registry, "externals", json!({}));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("load_catalog") && text.contains("inventory"),
            "the error must name the tools that fix it: {text}"
        );
    }

    /// One loaded catalog is the default, so an agent working on a single
    /// program never names it.
    #[test]
    fn one_loaded_catalog_answers_without_being_named() {
        let mut registry = one_catalog();
        assert_eq!(
            call(&mut registry, "callers", json!({ "name": "b" }))["isError"],
            false
        );
    }

    /// With several loaded, picking one would answer confidently about the
    /// wrong program. The error names them instead.
    #[test]
    fn several_loaded_catalogs_must_be_told_apart() {
        let mut registry = Registry::with_sessions(vec![
            ("first", session_from(&[("a", "b")])),
            ("second", session_from(&[("c", "d")])),
        ]);

        let text = call_text(&mut registry, "callers", json!({ "name": "b" }));
        assert!(
            text.contains("first") && text.contains("second"),
            "the error must name the loaded catalogs: {text}"
        );

        // Named, each answers about its own program and not the other's.
        let first = call(
            &mut registry,
            "callers",
            json!({ "name": "b", "catalog": "first" }),
        );
        assert_eq!(first["isError"], false);
        let second = call_text(
            &mut registry,
            "defs",
            json!({ "name": "b", "catalog": "second" }),
        );
        let second: Value = serde_json::from_str(&second).unwrap();
        assert!(
            second["results"].as_array().unwrap().is_empty(),
            "`b` is defined in `first`, not `second`: {second}"
        );
    }

    #[test]
    fn a_catalog_named_but_never_loaded_is_a_tool_error() {
        let mut registry = one_catalog();
        let text = call_text(
            &mut registry,
            "externals",
            json!({ "name": "a", "catalog": "absent" }),
        );
        assert!(
            text.contains("absent") && text.contains("first"),
            "the error must name what was asked for and what is loaded: {text}"
        );
    }

    #[test]
    fn unloading_frees_the_catalog_and_leaves_the_rest() {
        let mut registry = Registry::with_sessions(vec![
            ("first", session_from(&[("a", "b")])),
            ("second", session_from(&[("c", "d")])),
        ]);
        let text = call_text(
            &mut registry,
            "unload_catalog",
            json!({ "catalog": "first" }),
        );
        assert!(text.contains("second"), "{text}");

        // One left, so it is the default again.
        assert_eq!(
            call(&mut registry, "externals", json!({}))["isError"],
            false
        );
        let text = call_text(&mut registry, "externals", json!({ "catalog": "first" }));
        assert!(text.contains("no catalog loaded as `first`"), "{text}");
    }

    #[test]
    fn listing_reports_every_loaded_catalog_with_its_analysis() {
        let mut registry = one_catalog();
        let text = call_text(&mut registry, "list_catalogs", json!({}));
        let listed: Value = serde_json::from_str(&text).unwrap();
        let catalogs = listed["catalogs"].as_array().unwrap();
        assert_eq!(catalogs.len(), 1);
        assert_eq!(catalogs[0]["catalog"], "first");
        assert!(
            catalogs[0]["analysis"].is_object() && catalogs[0]["scope"].is_object(),
            "a listing must carry the same honesty blocks a query answer does: {listed}"
        );
    }

    #[test]
    fn a_load_that_cannot_read_its_path_is_a_tool_error_not_a_protocol_error() {
        let mut registry = Registry::new();
        let result = call(
            &mut registry,
            "load_catalog",
            json!({ "path": "/nonexistent/catalog.json" }),
        );
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn a_management_call_missing_its_argument_is_a_tool_error() {
        let mut registry = Registry::new();
        let text = call_text(&mut registry, "load_catalog", json!({}));
        assert!(text.contains("`path`"), "{text}");
    }

    /// `indirect_targets` with a location missing its `:line` is a tool
    /// error the client can see, not an empty `results` list that would be
    /// byte-identical to a valid line with no indirect calls.
    #[test]
    fn an_unparseable_location_is_a_tool_error_not_an_empty_answer() {
        let mut registry = one_catalog();
        let result = call(
            &mut registry,
            "indirect_targets",
            json!({ "at": "parser.c" }),
        );
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("parser.c"),
            "the error must name the location it could not parse: {text}"
        );
    }

    #[test]
    fn a_valid_location_still_answers() {
        let mut registry = one_catalog();
        let result = call(
            &mut registry,
            "indirect_targets",
            json!({ "at": "parser.c:8" }),
        );
        assert_eq!(result["isError"], false);
    }

    #[test]
    fn a_json_value_that_is_not_a_request_object_is_invalid_request() {
        let response =
            handle_line(&mut one_catalog(), "42").expect("a non-object must still be answered");
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn a_request_without_a_method_is_invalid_request() {
        let response = handle_line(&mut one_catalog(), r#"{"jsonrpc":"2.0","id":1}"#)
            .expect("a request with an id must be answered");
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], 1);
    }

    #[test]
    fn a_notification_is_still_never_answered() {
        assert!(
            handle_line(
                &mut one_catalog(),
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
    }
}
