# Call-graph overlay + rllvm Example

`dispatch` calls through `o->on_event`, which LLVM cannot bound, so `reach
main handler` finds no path. An agent that reads the code records the edge in
an overlay beside the catalog; `--include-overlay` walks it, labeled `agent`,
and the answer says it is not proven.

![rllvm-query finds no path from main to handler, an agent records the ops@8 edge in an overlay, and reach with --include-overlay returns the path labeled agent and not proven; an annotate verdict then rides along on reach --annotations](../../docs/assets/callgraph-overlay-cli.gif)

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`.

## Build and verify

```bash
./check.sh
```

It checks the candidate, the path with and without the overlay, the slice
through the edge and the module emitted from it, a retraction, compaction, and
the same loop over [MCP](../mcp/): `record_edges`, `reach` with
`include_overlay`, then `save_overlay`.

## What it does

```bash
rllvm-query --catalog build/catalog.json resolution-candidates  # ops@8: handler
echo '{"op":"add","via_field":{"record":"ops","offset":8},"to":"handler","confidence":"high","provenance":["ops.c:8: o->on_event = handler"]}' \
  | rllvm-query --catalog build/catalog.json overlay record
rllvm-query --catalog build/catalog.json reach main handler --include-overlay
rllvm-query --catalog build/catalog.json slice main handler --include-overlay --emit-module build/slice.bc
```

The path's last step is `agent`, `dispatch -> handler at ops.c:10 via ops@8
[high, unverified]`, and the answer ends `not proven: this path uses 1 agent
edge(s)`. The slice is `main`, `dispatch` and `handler`, and `build/slice.bc`
defines those three and nothing else. Without `--include-overlay`, neither
query reads the overlay.

## The overlay file

- It is JSON lines, `<catalog stem>.overlay.jsonl` unless `--overlay PATH`
  names another. The catalog itself never changes.
- `overlay record` validates a batch and appends it, all or none. `overlay
  list` shows edges, annotations and verdicts; `overlay compact` rewrites the
  file as what now stands.
- An `add` names a field (`via_field`), a slot (`via_slot`: the global,
  parameter or accessor result a call reads its callee from) or one `site`. A
  field or slot covers every unresolved site through it, and an edge attaches
  only where LLVM left the call unbounded. `verify` and `retract` name an
  edge's key.
- The file is bound to one build. After a rebuild it refuses to load, naming
  both fingerprints, as it does when an rllvm-query that numbers call sites
  differently wrote it. A write refuses if another writer changed the file
  since it was read. To start again, move or delete it, or name another with
  `--overlay` (MCP: `path`).
- An `annotate` record holds a verdict on a value rather than an edge; see the
  [value guards example](../value-guards/).

## Walking it

```bash
rllvm-query --catalog build/catalog.json closure handler in --include-overlay --min-confidence medium
```

- `--include-overlay` makes `reach`, `closure` and `slice` also walk overlay
  edges, never refuted ones. `--min-confidence low|medium|high` (default
  `low`) drops weaker ones.
- Each step through one is labeled `agent` with the edge's provenance, a path
  or slice using one ends `not proven`, and `closure` lists what only agent
  edges reach apart.
- An overlay file that cannot be read is an error, never a direct-only answer,
  as is a missing file that `--overlay` names.
- `slice --emit-module` writes the slice's definitions as one small module, to
  check an edge in scope. It is cut out with the `llvm-extract` and `llvm-nm`
  in the configured `llvm_bindir` and joined with the configured `llvm-link`.
  An alias comes with the function it stands for; an empty slice writes
  nothing.
