//! MCP stdio server: the nine source-level queries as JSON-RPC 2.0 tools,
//! newline-delimited over stdin/stdout.
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
//! Tool names (`tool_for`, `query_from_call`) are `snake_case`, matching
//! `Query`'s own serde `kind` tag, so a result envelope's `"kind":
//! "indirect_targets"` feeds straight back into `tools/call`'s `name`. CLI
//! subcommands (`cli::QueryCommand`) are `kebab-case` instead, clap's
//! convention. The two spellings diverge only in these literals; nothing
//! ties them together.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::error::Error;

use super::{Direction, Query, Session, run};

/// The modern protocol revision this server has been checked against.
const MODERN: &str = "2026-07-28";
/// The legacy protocol revision an `initialize` handshake may select.
const LEGACY: &str = "2025-06-18";

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

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
/// `session` is loaded once by the caller before this is entered, so the
/// first `tools/call` pays no analysis cost and every call after it answers
/// from memory.
pub fn serve(
    session: &Session,
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
        if let Some(response) = handle_line(session, trimmed) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

/// Handle one request line. Returns `None` only for a notification: a
/// request object carrying a `method` and no `id`. Everything else is
/// answered, including a value that is not a request object at all.
fn handle_line(session: &Session, line: &str) -> Option<Value> {
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
        ("tools/call", _) => tool_call_outcome(session, &params),
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
    json!({ "tools": sample_queries().iter().map(tool_for).collect::<Vec<_>>() })
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
    json!({ "type": "string", "description": "Symbol to look up" })
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

/// The MCP tool definition for one `Query` variant: name, description, and
/// JSON input schema. Exhaustive over `Query` with no wildcard arm, and the
/// name is taken from `tool_name_of` so the listing and the name a client
/// calls back cannot be spelled differently.
fn tool_for(query: &Query) -> Value {
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
                    "from": { "type": "string", "description": "Symbol to start from" },
                    "to": { "type": "string", "description": "Symbol to reach" }
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

/// Runs a `tools/call`. `name`/`arguments` are validated by `query_from_call`;
/// a name or arguments that do not resolve to a query, and a query the
/// session cannot interpret, both become a `CallToolResult` with
/// `isError: true` rather than a protocol error, so the client can display
/// what was wrong with the call it made.
fn tool_call_outcome(session: &Session, params: &Value) -> Outcome {
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

    let query = match query_from_call(name, &arguments) {
        Ok(query) => query,
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
    let string_field = |key: &str| -> Result<String, String> {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing or non-string argument `{key}`"))
    };

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
        assert_eq!(
            listed.len(),
            sample_queries().len(),
            "every listed tool must come from exactly one sampled variant"
        );
    }

    /// A tool name is one spelling on both sides: listed once, and the same
    /// literal `query_from_call` matches.
    #[test]
    fn no_two_tools_share_a_name() {
        let mut listed = listed_tool_names();
        listed.sort();
        let count = listed.len();
        listed.dedup();
        assert_eq!(listed.len(), count, "duplicate tool name in tools/list");
    }

    /// `indirect_targets` with a location missing its `:line` is a tool
    /// error the client can see, not an empty `results` list that would be
    /// byte-identical to a valid line with no indirect calls.
    #[test]
    fn an_unparseable_location_is_a_tool_error_not_an_empty_answer() {
        let session = session_from(&[("a", "b")]);
        let call = json!({ "name": "indirect_targets", "arguments": { "at": "parser.c" } });
        let Outcome::Result(result, _) = tool_call_outcome(&session, &call) else {
            panic!("a query error must be a CallToolResult, not a protocol error");
        };
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("parser.c"),
            "the error must name the location it could not parse: {text}"
        );
    }

    #[test]
    fn a_valid_location_still_answers() {
        let session = session_from(&[("a", "b")]);
        let call = json!({ "name": "indirect_targets", "arguments": { "at": "parser.c:8" } });
        let Outcome::Result(result, _) = tool_call_outcome(&session, &call) else {
            panic!("a valid call must produce a result");
        };
        assert_eq!(result["isError"], false);
    }

    #[test]
    fn a_json_value_that_is_not_a_request_object_is_invalid_request() {
        let session = session_from(&[("a", "b")]);
        let response = handle_line(&session, "42").expect("a non-object must still be answered");
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn a_request_without_a_method_is_invalid_request() {
        let session = session_from(&[("a", "b")]);
        let response = handle_line(&session, r#"{"jsonrpc":"2.0","id":1}"#)
            .expect("a request with an id must be answered");
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], 1);
    }

    #[test]
    fn a_notification_is_still_never_answered() {
        let session = session_from(&[("a", "b")]);
        assert!(
            handle_line(
                &session,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
    }
}
