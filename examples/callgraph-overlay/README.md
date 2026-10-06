# Call-graph overlay + rllvm Example

`dispatch` calls through `o->on_event`, which LLVM cannot bound, so `reach
main handler` finds no path. An agent that reads the code records the edge in
an overlay beside the catalog; `--include-overlay` walks it, labeled `agent`,
and the answer says it is not proven.

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`.

## Build and verify

```bash
./check.sh
```

It checks the candidate, the path with and without the overlay, a retraction,
compaction, and the same loop over MCP: `record_edges`, `reach` with
`include_overlay`, then `save_overlay`.

## What it does

```bash
rllvm-query --catalog build/catalog.json resolution-candidates  # ops@8: handler
echo '{"op":"add","via_field":{"record":"ops","offset":8},"to":"handler","confidence":"high","provenance":["ops.c:8: o->on_event = handler"]}' \
  | rllvm-query --catalog build/catalog.json overlay record
rllvm-query --catalog build/catalog.json reach main handler --include-overlay
```

The path's last step is `agent`, `dispatch -> handler at ops.c:10 via ops@8
[high, unverified]`, and the answer ends `not proven: this path uses 1 agent
edge(s)`. Without `--include-overlay`, `reach` never reads the overlay.
