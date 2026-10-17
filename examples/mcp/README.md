# MCP server + rllvm Example

`rllvm-query mcp` serves the same queries as JSON-RPC 2.0 tools over stdio.

## Requirements

`rllvm-cc` to capture the fixture, and `rllvm-query` — a separate crate that a
default `cargo build` does not build.

## Build and verify

```bash
./check.sh
```

It captures `lib.c`, then `mcp_session.py` runs two full sessions over stdio —
one per protocol era the server supports. Each session discovers the server,
lists its tools, loads the captured module with `inventory`, and asks `defs`
where `helper` is defined. The check confirms the answer (`lib.c:1`), that the
query answer carries the aggregate analysis counts but not the per-module
roster (which rides `load_catalog` and `list_catalogs`), that the modern
session's cache envelope is well formed (`cacheScope` is `public` or
`private`), and that stdout carried protocol frames and nothing else.

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

## Stdout is the protocol

Diagnostics go to stderr. Anything else on stdout would corrupt the frame
stream for a client reading it, which is why the check parses every line as
JSON.
