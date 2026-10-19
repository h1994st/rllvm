# Queries + rllvm Example

`rllvm-query` answers source-level questions about captured bitcode.

## Requirements

`rllvm-query`, a separate crate that links LLVM statically and is not built by
a default `cargo build`:

```bash
cargo install rllvm-query
```

## Build and verify

```bash
./check.sh
```

It builds two translation units, writes a catalog, and checks `defs`,
`callers` and `callees` agree about the same program.

## What it does

```bash
rllvm-info build/app --json > build/catalog.json
rllvm-query --catalog build/catalog.json defs helper
rllvm-query --catalog build/catalog.json callers helper
rllvm-query --catalog build/catalog.json callees main
```

The others are `at`, `uses`, `reach`, `slice`, `closure`, `externals`,
`ffi-exports`, `indirect-targets`, `resolution-candidates` and `guards`.

Each invocation loads the whole catalog. Piped in, one per line, several
queries share one load. Every line is checked before the catalog is read, and
each answer follows a `== <query>` line, or is one line of JSON with `--json`:

```bash
printf '%s\n' 'defs helper' 'callers helper' \
  | rllvm-query --catalog build/catalog.json
```

## Reading an answer

Every answer carries the results **and** what it could not see: which modules
failed to parse, which call sites are indirect, which symbols bind
ambiguously, and whether each location's source has changed since it was
compiled. Answers print as text, with anything that bears on the answer under
`note:`. `--json` gives the full envelope, and `--full` prints its scope,
analysis, uncertainty and provenance blocks as text. `analysis` is what was
actually read; `scope` is quoted from the catalog and never shrinks. The MCP
server always speaks JSON.

An indirect call site, and an address stored or placed in a table, names the
record field it goes through (`via ops@8 (on_event)`, `into ops@8`) when the
IR proves one; the member name needs `-g`.

An empty `reach` is not a proof of unreachability, and `callees` is an upper
bound rather than a reachable set.

## The facts cache

Extracted per-module facts are cached. Each answer's `analysis.cache` (and
`--full`) reports hits, misses and disk use; `RLLVM_QUERY_CACHE=0` turns it
off. Past `query_cache_warn_mb`, an answer adds a footer note naming
`rllvm-query cache clear`.

```bash
rllvm-query cache               # location and disk use
rllvm-query cache clear         # delete every cached entry
rllvm-query cache clear --stale # delete only generations this binary no longer reads
```
