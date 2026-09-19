# MCP server + rllvm Example

`rllvm-query mcp` serves the same queries as JSON-RPC 2.0 tools over stdio.

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`.

## Build and verify

```bash
./check.sh
```

It sends one `tools/list` request and checks the reply lists the tools — and
that stdout carried protocol frames and nothing else.

## What it does

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | rllvm-query mcp
```

A real client keeps the session open, chooses what to analyse with
`load_catalog` (a catalog JSON) or `inventory` (a binary, archive or `.bc`),
and can keep several loaded at once. Each is analysed once and answers from
memory after that.

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
stream for a client reading it, which is why this example checks every line
parses as JSON.
