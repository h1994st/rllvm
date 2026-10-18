# Claude Code plugin + rllvm Example

The plugin in [`plugins/rllvm`](../../plugins/rllvm/) bundles the
`rllvm-query` MCP server with skills for setting rllvm up, capturing bitcode,
and choosing and reading queries.

## Install

```text
/plugin marketplace add h1994st/rllvm
/plugin install rllvm@rllvm
```

`rllvm` and `rllvm-query` are installed separately; the `setup` skill walks
through it, and picks an LLVM every reader of the bitcode accepts.

## What it adds

- `/rllvm:complete-callgraph` runs the `complete-callgraph` skill on a
  catalog.
- `/overlay-view` opens a read-only pane for the catalog that last answered:
  each unresolved field, its candidates, and the overlay's edges with
  confidence and verification, unsaved ones included.
- A `PostToolUse` hook notes once per session when the facts cache passes
  `query_cache_warn_mb`. It needs `jq`.

## Requirements

`rllvm-query` and `python3`. `claude` is optional: when present, the check
also validates the manifests. Without `jq` the cache hook is not exercised.

## Build and verify

```bash
./check.sh
```

It starts the server the plugin declares and checks the tools it lists, runs
both scripts against good and broken configurations, checks every skill's
header, and checks the cache hook warns once per session.
