# Value guards + rllvm Example

`copy` hands `put` the length `n = len < CAP ? len : CAP`. `guards` finds the
clamp that bounds it, and says nothing is known about a value nothing
constrains: no guard is an empty list, never "unbounded". In `offset.c`,
`reserve` returns early when `left > MAX` and hands `make_buf` `left + 1`;
`guards` carries the bound to that argument through their affine relation. In
`field.c`, `fill` stores the clamp to `b->len` and reads it back for `put`;
`guards` follows the load to the store.

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`,
and `opt` in the configured `llvm_bindir`: `guards` reads the anchor's module
through it.

## Build and verify

```bash
./check.sh
```

It checks the clamp from a call site and from a named local, the empty answer
for an unconstrained argument, the module `--emit-module` writes, a recorded
verdict, an annotated `reach` that keeps its path, the verified bound on
`left + 1`, and the verified clamp through `b->len`.

## What it does

```bash
rllvm-cc -g -O0 clamp.c -o build/clamp
rllvm-info build/clamp --json > build/catalog.json
rllvm-query --catalog build/catalog.json guards --at clamp.c:14 --operand 2 --from copy
rllvm-query --catalog build/catalog.json guards --function copy --local n
```

The first names argument 2 of the call on line 14 (`--operand` counts from 0;
`--return` names the call's result instead). The answer lists a `clamp` guard
with bound 64 at `clamp.c:13`. It `defines` the value, producing it rather
than only branching on it, and is `on the anchor` (`depth` 0): it bounds the
value itself, not one the value is computed from. `--json` gives each guard's
`op`, `relation` and `depth` under `results.guards[]`, and `--emit-module
OUT.bc` writes the slice behind the answer. A line holding several calls is
named by the call site id the error prints.

The second names the same value as the `-g` local `n` in `copy`, which needs
no call site, and finds the same clamp.

```bash
rllvm-cc -g -O0 offset.c -o build/offset
rllvm-info build/offset --json > build/offset.json
rllvm-query --catalog build/offset.json guards --at offset.c:15 --operand 1
```

This names `left + 1`, the size handed to `make_buf`. The answer lists the
`compare ugt 64` on `left` at `offset.c:13`, one step upstream, with
`anchored = guarded + 1`: in `--json`, `affine` is
`{"scale": "1", "offset": "1"}`, decimal strings, and `verified` is `true`.
Past the early return `left` is in `[0, 64]`, and `[1, 65]` stays within a
`size_t`.

```bash
rllvm-cc -g -O0 field.c -o build/field
rllvm-info build/field --json > build/field.json
rllvm-query --catalog build/field.json guards --at field.c:19 --operand 2
```

This names `b->len`, loaded for `put` right after line 18 stores the clamp
there. Nothing between the store and the load writes memory, so the load is
the stored value: the answer lists the `clamp umin 64` at `field.c:18`, on the
anchor and `verified`. A call between them (say `log(b)`) could change the
field, so the same clamp would come back unverified, its caveat naming the
call.

## Reading a guard

- `relation`: the guard `defines` the value; `dominates` its use, running
  first on every path whichever way it branches; or is only `on_path` to it.
- `depth` 0 is the value itself, or a value it only widens: a bound holds
  across `zext` or `sext` when the guard's signedness matches the cast's.
  Depth N is N steps up the chain the value is computed from, a truncation
  counting as a step, and is not by itself a bound on the value.
- `affine` is that chain as `anchored = scale * guarded + offset` when it is
  constant `add`, `sub`, `mul` and `shl` alone, or a matching widening
  (`{1, 0}`). `verified` is `true` when the relation is affine and every value
  the guard admits maps onto the anchored value without wrapping at its width.
  Otherwise the guard is an unverified lead, and `caveat` says why. A clamp
  that runs on the value and is used later bounds its own result, not the
  value, so it is unverified too.
- `verified` does not say the guard runs on every path; `relation` does. A
  bound holds unconditionally only when it is `verified` and `dominates` or
  `defines` the value.
- The numeric cap is `bound`. A `verified` guard without one, such as a clamp
  to another variable, is capped by a runtime value, the guard's operand: a
  sound relation, not a numeric limit. To reach a number, anchor on that
  operand and trace it.
- A comparison admits the side that holds on its branch's edge into the
  anchor: past `if (left > MAX) return;`, `left` is in `[0, MAX]`. With no
  such edge (the block it enters is also entered from elsewhere, or does not
  dominate the use), or no constant bound, nothing is known to carry, and a
  guard upstream of arithmetic is unverified.
- The trace stays in the anchor's function. Where the value enters through a
  parameter or leaves a call, `crossings` name the call to ask `guards` about
  next. `--from F` keeps only callers on a path from `F`, which must name one
  function, and examines nothing between them.
- A load is followed to the store it reads back: a non-volatile store of
  the same type to the same address, the same pointer or a `getelementptr`
  alike, that runs before it. With exactly one, in the load's block, and only
  operations that write no memory between them, the load is the stored
  value. A call, another store or any other operation between, more than one
  such store, or a store in another block still leads on, but every guard
  reached that way is unverified, its caveat saying which.
- Where the trace cannot follow the value (a load with no such store, or a
  volatile or atomic one, a pointer round trip, inline assembly, an
  intrinsic it does not read through), `uncertainty.value_trace_lost` says
  where, and the list is partial.
- Over MCP, `guards` needs a program loaded with `load_catalog`.

## Recording a verdict

```bash
echo '{"op":"annotate","anchor":{"kind":"argument","site":"clamp.c:14","operand":2},"verdict":"infeasible","confidence":"high","provenance":["clamp.c:13: n = len < CAP ? len : CAP"]}' \
  | rllvm-query --catalog build/catalog.json overlay record
echo '{"op":"verify","annotation":{"anchor":{"kind":"argument","site":"clamp.c:14","operand":2}},"tool":"review","verdict":"confirmed"}' \
  | rllvm-query --catalog build/catalog.json overlay record
rllvm-query --catalog build/catalog.json overlay list
rllvm-query --catalog build/catalog.json reach main put --annotations
```

The clamp is evidence; whether a length above `CAP` can reach `put` is a
verdict you write as an `annotate` record in the
[call-graph overlay](../callgraph-overlay/) beside the catalog.

- Its `verdict` is `feasible`, `infeasible` or `conditional`, with a
  `confidence` and `provenance`. It is never proof, and rllvm-query never
  writes one itself.
- Its `anchor` names a value as `guards` does: an `argument` (`site`,
  `operand`), a `return` (`site`) or a `local` (`function`, `name`). A `site`
  is a `file:line` or a call site id. An optional `from` names the one
  function the verdict holds for.
- Recording reads the anchor's module through `opt`; an anchor that does not
  resolve, or is ambiguous, rejects the batch.
- `verify` and `retract` name it by its `anchor` and `from` under
  `annotation`. `overlay list` shows it with its verdict and verification;
  with `--json`, and in MCP's `list_overlay`, it sits under `annotations`
  beside `edges`.

`reach main put --annotations` returns the same path as without the flag, and
the annotation rides along under `uncertainty.annotated_flows`, labeled
`agent`: a hypothesis about the call to `put`, never proof that the path
cannot run.

- `--annotations` works on `reach` and `slice` (MCP: `include_annotations`,
  default off), independent of `--include-overlay`.
- It surfaces each infeasible or conditional annotation, not refuted, whose
  anchor lies on the returned path. `annotated_flows` is omitted from the JSON
  when none attach.
- An annotation with a `from` attaches only when the walk's `from` names that
  same single function; a `from` matching several functions shows none.
- The flag is an error when no overlay is attached (a missing file, or over
  MCP none loaded) or the overlay holds no annotations.
