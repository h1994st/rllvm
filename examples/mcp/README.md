# MCP server + rllvm Example

`rllvm-query mcp` serves the same queries as JSON-RPC 2.0 tools over stdio.
Answers are always the JSON envelope.

## Requirements

`rllvm-cc` to capture the fixture, and `rllvm-query` — a separate crate that a
default `cargo build` does not build.

## Build and verify

```bash
./check.sh
```

It captures `lib.c`, then `mcp_session.py` runs one session per protocol era
the server supports: discover the server, list its tools, load the module with
`inventory`, and ask `defs` where `helper` is defined. It checks:

- the answer is `lib.c:1`;
- it carries the aggregate analysis counts but not the per-module roster,
  which rides `load_catalog` and `list_catalogs`;
- the modern session's cache envelope is well formed (`cacheScope` is
  `public` or `private`);
- stdout carried protocol frames and nothing else.

## What a session looks like

A client keeps one connection open and sends newline-delimited requests:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"server/discover"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"inventory","arguments":{"artifact":"lib.o"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"defs","arguments":{"name":"helper"}}}' |
    rllvm-query mcp
```

It chooses what to analyze with `inventory` (a binary, archive or `.bc`) or
`load_catalog` (a catalog JSON), and can keep several loaded at once; each is
analyzed once and answers from memory after that. A modern client adds an
`_meta` block naming the protocol version it speaks; `server/discover` lists
the versions the server supports.

## Pointing a client at it

```json
{
  "mcpServers": {
    "rllvm": {
      "command": "rllvm-query",
      "args": ["mcp"]
    }
  }
}
```

In Claude Code, the [plugin](../claude-plugin/) configures this for you.

## The overlay over MCP

Four tools keep the [call-graph overlay](../callgraph-overlay/):

- `record_edges` takes the records `overlay record` reads, all or none,
  attaching the overlay beside the catalog first if none is.
- `save_overlay` is the only tool that writes, appending to the file.
- `load_overlay` attaches the default file or `path` (required after
  `inventory`), and refuses to drop unsaved records unless `discard_pending`
  is set.
- `list_overlay` shows the edges and annotations, saved or not.

Recorded edges are walked at once: `reach`, `closure` and `slice` take
`include_overlay` and `min_confidence`, and `reach` and `slice` take
`include_annotations`. `slice`'s `emit_module`, `guards`, and an `annotate`
record need a catalog from `load_catalog`. `unload_catalog` reports any
unsaved records it dropped.

## Stdout is the protocol

Diagnostics go to stderr. Anything else on stdout would corrupt the frame
stream for a client reading it, which is why the check parses every line as
JSON.
