# Value guards + rllvm Example

`copy` hands `put` the length `n = len < CAP ? len : CAP`. `guards` finds the
clamp that bounds it, and says nothing is known about a value nothing
constrains: no guard is an empty list, never "unbounded".

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`,
and `opt` in the configured `llvm_bindir`.

## Build and verify

```bash
./check.sh
```

It checks the clamp from a call site and from a named local, the empty answer
for an unconstrained argument, and the module `--emit-module` writes.

## What it does

```bash
rllvm-cc -g -O0 clamp.c -o build/clamp
rllvm-info build/clamp --json > build/catalog.json
rllvm-query --catalog build/catalog.json guards --at clamp.c:14 --operand 2 --from copy
rllvm-query --catalog build/catalog.json guards --function copy --local n
```

The first names argument 2 of the call on line 14 (`--operand` counts from
0). The answer lists a `clamp` guard with bound 64 at `clamp.c:13`, and its
relation is `defines`: the clamp produces the value rather than only branching
on it. It is `on the anchor` (`depth` 0 in `--json`): it bounds the value
itself, not one the value is computed from. `--json` gives the same as
`results.guards[].op`, `relation` and `depth`. `--emit-module` writes the
slice that holds the guard.

The second names the same value as the source variable `n` in `copy`, which
needs no call site but a module built with `-g`, and finds the same clamp.
