# Queries + rllvm Example

`rllvm-query` answers source-level questions about captured bitcode.

## Requirements

The `query` feature, which links LLVM statically and is not in a default
build:

```bash
cargo install rllvm --features query
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

The other six are `at`, `uses`, `reach`, `closure`, `externals` and
`indirect-targets`.

## Reading an answer

Every answer is one JSON envelope carrying the results **and** what the answer
could not see: which modules failed to parse, which call sites are indirect,
which symbols bind ambiguously, and whether each location's source has changed
since it was compiled. `analysis` reports what was actually read; `scope` is
quoted from the catalog and never shrinks.

An empty `reach` is not a proof of unreachability, and `callees` is an upper
bound rather than a reachable set.
