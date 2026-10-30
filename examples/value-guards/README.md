# Value guards + rllvm Example

`copy` hands `put` the length `n = len < CAP ? len : CAP`. `guards` finds the
clamp that bounds it, and says nothing is known about a value nothing
constrains: no guard is an empty list, never "unbounded". In `offset.c`,
`reserve` returns early when `left > MAX` and hands `make_buf` `left + 1`;
`guards` carries the bound to that argument through their affine relation. In
`field.c`, `fill` stores the clamp to `b->len` and reads it back for `put`;
`guards` follows the load to the store, and in `refill`, which stores the field
twice, to the nearer one. In `reuse.c`, `fill` reuses the local `cur` for two
values; `--at` traces the one it holds at each use. `drain` shadows `cur`;
`--declared-at` picks a variable by the line it is declared on. In `clamp.c`,
`peek` clamps `len` into `n`; a guard list on `len` there, or in `copy`, also
gives the comparison the clamp is made of. `trim` clamps through a
`static inline` `min_size` helper, which `-O0` keeps out of line; `guards`
reads the call as the clamp it computes.

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
`left + 1`, the verified clamp through `b->len` and through the nearer of
`refill`'s two stores, a reused local traced at each of its uses, a shadowed
local picked by its declaration line, and the comparison a downstream clamp is
made of: verified on its branch's edge into the call in `peek`, a lead in
`copy`, and the clamp a call to `min_size` computes in `trim`.

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
`compare ule 64` on `left` at `offset.c:13`, one step upstream, with
`anchored = guarded + 1`: in `--json`, `affine` is
`{"scale": "1", "offset": "1"}`, decimal strings, and `verified` is `true`. The
source tests `left > MAX`, but the call is past the early return, on the false
edge, where `left ule 64` holds: `left` is in `[0, 64]`, and `[1, 65]` stays
within a `size_t`.

```bash
rllvm-cc -g -O0 field.c -o build/field
rllvm-info build/field --json > build/field.json
rllvm-query --catalog build/field.json guards --at field.c:19 --operand 2
```

This names `b->len`, loaded for `put` right after line 18 stores the clamp
there. Nothing between the store and the load writes memory, so the load is
the stored value: the answer lists the `clamp umin 64` at `field.c:18`, on the
anchor and `verified`. A call between them (say `note(b)`) could change the
field, so the same clamp would come back unverified, its caveat naming the
call.

```bash
rllvm-query --catalog build/field.json guards --at field.c:25 --operand 2
```

`refill` stores `0` to `b->len`, then the clamp. The load for `put` reads back
the nearer store, which overwrites the first, so the same clamp comes back
`verified`.

```bash
rllvm-query --catalog build/catalog.json guards --at clamp.c:26 --operand 2
rllvm-query --catalog build/catalog.json guards --at clamp.c:30 --operand 2
```

In `peek`, an `if` clamps `len` to 64 into `n`, and line 26 passes `len` itself
from inside the branch where `len < 64`. The clamp bounds `n`, which is
computed from `len`, so it comes back unverified; the `compare ult 64` it is
made of is listed beside it and `verified`, since every path to the call
crosses its true edge. At line 30 `n` is the clamp's result: one verified
clamp, its comparison not listed again.

```bash
rllvm-query --catalog build/catalog.json guards --function copy --local len
```

Traced from `len` in `copy`, `copy`'s own clamp comes back unverified, and its
`compare ult 64` is a lead, its caveat "no branch edge shows the comparison
held at the use": the ternary's arms join before `n` is used, so no edge says
which side holds at `len`.

## A min helper

```bash
rllvm-query --catalog build/catalog.json guards --at clamp.c:37 --operand 2
```

`trim` hands `put` `n = min_size(len, CAP)`. At `-O0` the `static inline` helper
stays a call, but its body only compares its two parameters and returns one, so
the call is read as the clamp it computes: `clamp umin 64` at `clamp.c:36`,
defining the value, `verified`, with `via min_size` naming the helper (in
`--json`, `via` holds its mangled symbol, and `symbols` its reading). No
crossing out of `min_size` is listed. A helper that is `extern` or `weak`,
defined in another module, or that casts, loads, stores or calls is a crossing,
as any other call is. `-Og` inlines such helpers, so capture there when their
bounds matter more than a faithful call graph.

## A reused local

In `reuse.c`, `fill` hands `put` the local `cur`, first holding `head` and
then `tail`, each checked before the call. One local, two values:

```bash
rllvm-cc -g -O0 reuse.c -o build/reuse
rllvm-info build/reuse --json > build/reuse.json
rllvm-query --catalog build/reuse.json guards --function fill --local cur
rllvm-query --catalog build/reuse.json guards --function fill --local cur --at reuse.c:13
rllvm-query --catalog build/reuse.json guards --function fill --local cur --at reuse.c:17
```

Named alone, `cur` traces nothing: `value_trace_lost` says it is bound to more
than one value and lists the lines that use it. `--at` names one of those
lines and traces the value `cur` holds there. A use at a line is a read of the
value the local holds there, possibly through another variable that holds the
same value inside the local's scope; a read inside code inlined from a callee
counts at the line that calls it. Traced at line 13, it finds
`compare ule 32` (line 11); traced at line 17, `compare ule 8` (line 15); each
dominates its use, reached past its `return`. A line with no use of `cur` is an
error listing the lines that have one, and a line reading two of its values is
an error rather than a pick. An overlay `annotate` anchor names a local without
a use-site, so a reused local cannot be annotated.

## A shadowed local

`drain` declares `cur` on line 21, holding `head`, and again on line 25 inside
a block, holding `tail`. One name, two variables:

```bash
rllvm-query --catalog build/reuse.json guards --function drain --local cur
rllvm-query --catalog build/reuse.json guards --function drain --local cur --declared-at reuse.c:25 --at reuse.c:28
rllvm-query --catalog build/reuse.json guards --function drain --local cur --declared-at reuse.c:21 --at reuse.c:30
```

Named alone, `cur` is an error naming the line each is declared on.
`--declared-at` names one of those lines and picks that variable, and `--at`
then reads it at a use as for a reused local: at line 28 the inner `cur`,
`compare ule 8` (line 26); at line 30 the outer, `compare ule 32` (line 22). A
line no `cur` is declared on is an error listing those that are, and a line two
are declared on, as one macro can declare both, is an error rather than a pick.
A variable of a function inlined into `drain` is not one of its own. An overlay
`annotate` anchor carries no declaration line either, so a shadowed local cannot
be annotated.

## Reading a guard

Each guard in the answer carries these fields:

| Field | Meaning |
| --- | --- |
| `op`, `predicate` | What the guard is: `compare ult`, `clamp umin`, `saturate uadd.sat`, `mask`, `modulo urem`, ... |
| `bound` | The numeric limit, when there is one. Without it, the limit is another value, the guard's operand: trace that operand to reach a number. |
| `relation` | Where the guard runs: it `defines` the value, `dominates` its use (runs first on every path), or is only `on_path` to it. |
| `depth` | 0 is the value itself, or a widening of it when the guard's signedness matches the cast. N is N steps up the computation, a truncation counting as one, and is not by itself a bound on the value. |
| `affine` | How the anchored value follows from the guarded one, `anchored = scale * guarded + offset`, for constant `add`, `sub`, `mul` and `shl`. |
| `verified` | `true` when the bound carries to the value. It holds on every path only when `relation` is `dominates` or `defines`; `on_path` holds only where the guard runs. |
| `caveat` | Why a guard is not verified. |
| `via` | The helper a clamp was read from. |

### When a guard is verified

- A comparison is verified when the use can only be reached through one side
  of a branch on it. Its `predicate` is that side, so it may be the negation of
  the source: past `if (left > MAX) return;` it is `ule MAX`. A loop's backedge
  into that side is fine. A comparison with no such branch keeps its predicate
  as written and is a lead.
- A clamp, mask, modulo or saturation is verified when its bound carries to the
  value. It holds on every path only when it also `dominates` or `defines` it.
- Arithmetic between the guard and the value must be affine, and no value the
  guard admits may wrap; otherwise the guard is a lead. A guard with no
  constant bound never carries through arithmetic.
- A clamp applied to the value later bounds its own result, not the value, so it
  is a lead. When that clamp is built from a comparison (a branch or `select`,
  not an intrinsic `umin`), the comparison is listed on its own, since it does
  bound the value.

### Where to anchor

- Anchor at the use. A `--return`, or a `--local` without `--at`, comes before
  the checks on the value, so they are only leads there: `--at` the later use
  of a local, or anchor on the call that consumes a returned value.
- The trace stays in the anchor's function. `crossings` name the call where the
  value enters or leaves; ask `guards` about it next. A call to a trivial `min`
  or `max` helper is read as its clamp instead.
- `--from F` keeps only callers on a path from `F`, which must name one
  function; nothing between them is examined.

### Loads and lost traces

- A load is followed to the store it reads back: one of the same type, to the
  same address (the same pointer, or a `getelementptr` alike), that runs before
  it. The nearest such store in the load's block wins when nothing between
  them writes memory.
- If something between them might (a call, another store, an `atomicrmw`, a
  `cmpxchg`, a volatile load), or the stores are only in other blocks, the
  guards reached through the load are leads, their `caveat` says why, and
  `uncertainty.value_trace_lost` says the field may hold something else.
- `uncertainty.value_trace_lost` says where the trace stopped: a load with no
  such store, a volatile or atomic access, a pointer round trip, inline
  assembly, or an intrinsic it does not read through. The list is then partial.
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
