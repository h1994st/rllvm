---
name: complete-callgraph
description: Resolve unresolved indirect calls (function pointers, callback structs) into a labeled call-graph overlay with rllvm-query. Use when reach/closure/callers miss paths through callbacks, or when asked to complete the call graph.
---

# Completing the call graph

rllvm-query cannot bound a call through a function pointer, so `reach`,
`closure` and `callers` miss paths that run through callbacks. You can read
the code and record the missing edges in an overlay, a file beside the
catalog. Every edge is a hypothesis you label with confidence and the source
lines behind it; the catalog never changes and no answer uses the overlay
unless asked. Load the program first (`load_catalog`, see the `query` skill).

## When

`uncertainty.indirect_call_sites` is non-zero and an answer depends on a
callback path: a `reach` found no path, or a path you need may go through a
function table, an ops struct or a registered handler. Without that
dependence, do not start; completing a whole program is rarely the goal.

## The loop

1. `load_catalog`, then `resolution_candidates`. It groups unresolved
   indirect sites by the record field they dispatch through, with `field`
   (`ops@8`: record and byte offset), `field_name`, `signature`, `sites`, and
   `candidates`: functions stored into that field, each with its
   `assignments` and whether `signature_matches`.
2. For each group, read the source of every assignment (`at` on its
   `file:line`, or the file itself). Confirm the member is `field_name` and
   the struct is the record type, not a same-named field of another struct.
   `uses` on a candidate lists every place its address is taken.
3. Decide the targets. A candidate is not a target until you have read the
   assignment. `single_candidate` is a hint to check, not an answer.
4. `record_edges` with `add` records (below): `via_field`, `to`, `confidence`
   and `provenance`, quoting each assignment as `file:line: text`.
5. `list_overlay` to see the edges and the sites they cover, then
   `save_overlay`. Records stay in memory until it runs.
6. Ask the question again with `include_overlay` on `reach`, `closure` or
   `slice`.

## Confidence

- `high`: the assignment stores this function into this field of this record
  type, and the signature matches.
- `medium`: one indirection away (a helper copies the table into the record)
  or the signature differs by a cast.
- `low`: a match by name or signature only.

`min_confidence` on a walk excludes edges below it.

## Disciplines

- Patterns, not sites: when a group has a `field`, record with `via_field` so
  the edge covers every unresolved site through that field. Use `site` only
  for a site with no field.
- Never record without provenance. It is required, and it is what a reader
  checks.
- Never call an agent edge proven. When reporting a path that uses one, say
  "not proven" and show each agent step with its `provenance` quoted.
- Edges attach only to unresolved indirect sites. An edge on a direct or
  bounded site is rejected, and a batch with one bad record applies none; the
  error names its index.
- `to` is an exact symbol naming one definition. For a `static` function
  defined in several modules, give `{"module_id", "symbol"}`; the error lists
  the candidates' module ids.
- A verdict records whether the edge exists. It persists when a later `add`
  updates confidence or provenance. To reset it, `retract` and then `add`
  again.

## Reading a path with the overlay

`uncertainty.agent_path_steps` counts agent steps; non-zero means the path is
not proven. Then rerun the same query without `include_overlay` to look for a
proven path. A breadth-first search can return a shorter or equal-length path
through agent edges even when a direct path exists, and reporting that one
under-claims. Report the proven path if there is one, else the agent path as
a hypothesis. `closure` lists functions reached only through agent edges in
`uncertainty.agent_reached`, and `uncertainty.overlay` gives `covered_sites`
of `unresolved_sites`: say how many sites remain unresolved.

## Saving

`save_overlay` is the only tool that writes. Save before `unload_catalog`,
before reloading, and before the session ends; unsaved records are not
written on exit. `load_overlay` refuses to replace one holding unsaved
records unless `discard_pending` is true. After a rebuild the overlay refuses
to load, naming both fingerprints; record again.

## Verification

For an edge a conclusion depends on, do not rely on the first reading:

1. `slice` from the caller to the target with `include_overlay` and
   `emit_module`, which writes the slice's definitions as one small `.bc`.
2. Re-read that module carefully, or run a scoped points-to analysis on it.
3. Record the result with a `verify` record: `tool` is `reread`, `svf` or
   `phasar`, `verdict` is `confirmed`, `refuted` or `inconclusive`. A refuted
   edge is skipped by every walk.

A `confirmed` verdict from `reread` is still your judgment, not proof.

## Records

`record_edges` takes these as `records`; `rllvm-query overlay record` takes
them as JSON lines on stdin.

```json
{"op":"add","via_field":{"record":"ops","offset":8},"to":"handler","confidence":"high","provenance":["ops.c:8: o->cb = handler"]}
{"op":"verify","edge":"<key from list_overlay>","tool":"reread","verdict":"confirmed"}
{"op":"retract","edge":"<key from list_overlay>","reason":"re-read: the assignment is dead code"}
```
