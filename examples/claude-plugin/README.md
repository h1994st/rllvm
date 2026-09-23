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
through it.

## Requirements

`rllvm-query` and `python3`. `claude` is optional: when present, the check
also validates the manifests.

## Build and verify

```bash
./check.sh
```

It starts the server the plugin declares and checks the tools it lists, runs
both scripts against good and broken configurations, and checks every skill's
header.
