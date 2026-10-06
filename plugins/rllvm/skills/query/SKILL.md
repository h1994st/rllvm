---
name: query
description: Choose the rllvm-query tool that answers a question about a captured program, and report its answer without over-claiming. Use for questions about definitions, callers, callees, reachability, function pointers or external calls in captured bitcode, and every time an rllvm-query answer is about to be reported.
---

# Querying captured bitcode

Load the program first with `load_catalog` or `inventory` (the `capture`
skill). With more than one catalog loaded, pass `catalog`.

Without the MCP server, the same queries run as
`rllvm-query --catalog catalog.json <query>`, with kebab-case names
(`indirect-targets`). Output is text with its caveats in a footer; `--json`
prints the envelope described below, and `--full` adds scope, analysis and
uncertainty to the text.

Each invocation reloads the whole catalog, which on a large program takes
seconds. For many queries, pipe them on stdin to `rllvm-query --catalog
catalog.json` with no query argument, one per line as written on the command
line: the catalog loads once, each text answer follows a `== <query>` line,
and `--json` prints one envelope per line. Every line is checked before the
catalog is read.

Loads also reuse each module's extracted facts from a persistent cache keyed
by its content hash, so a rerun over the same bitcode skips re-extraction.
`analysis.cache` (and `--full`) reports hits, misses and disk use; a cached
answer is identical to an uncached one. `rllvm-query cache` reports the
cache's location and per-generation disk use, and `rllvm-query cache clear
[--stale]` prunes it — `--stale` keeps this binary's own generation and
removes only ones an older or newer LLVM left behind.

When a warning about the facts cache appears, offer `rllvm-query cache clear
--stale` or `cache clear` and run neither without the user's approval.

## Choosing the query

| Question | Tool |
| --- | --- |
| Where is X defined, and in which configurations? | `defs` |
| What runs at `file:line`? | `at` |
| Who calls X? What does X call? | `callers`, `callees` |
| Where is X's address taken? | `uses` |
| Who can call X through a pointer? | `uses` for address-taken sites, then `indirect_targets` at each |
| What can this indirect call reach? | `indirect_targets` at the call site (`heuristics: true` adds the address-taken inventory) |
| Which functions could an unresolved indirect call reach, grouped by the record field it dispatches through? | `resolution_candidates` (`resolution-candidates`): candidates, never edges; no walk follows them |
| Can A reach B — for example, is a vulnerable function reachable? | `reach`; if it finds no path, `closure` to see where the search stopped |
| Every function on some path from A to B, and the edges among them | `slice`; `emit_module` (`--emit-module OUT.bc`) also writes their definitions as one module |
| Everything that reaches X, or that X reaches | `closure` with `direction` `in` or `out` |
| What does the program call outside itself? | `externals` |
| Which Rust functions can C call — the FFI surface? | `ffi_exports` |

A name can be the mangled symbol, the full demangled reading, or a bare
identifier. A bare identifier matches a C++ demangled reading that contains
it as a whole identifier; a C symbol has no reading, so a bare C name
resolves only to its own exact symbol. When a bare name matches nothing,
find the exact symbol with `at`, `callers` or `callees`, then query that.

## Recording resolved indirect calls

When you resolve an unresolved indirect call by reading the code, record it
in the call-graph overlay. The overlay is a file beside the catalog,
`<catalog stem>.overlay.jsonl` unless another is named; the catalog is never
changed. Records are the same over MCP and the command line:

- `{"op":"add","via_field":{"record":"ops","offset":8},"to":"h3","confidence":"high","provenance":["init: o->handler = h3"]}`
  attaches to every unresolved site through `ops@8`; `"site": <call site
  id>` instead names one. `to` is an exact symbol naming one definition
  (ODR copies count as one), or `{"module_id", "symbol"}`. `provenance` is
  required.
- `{"op":"verify","edge":<key>,"tool":...,"verdict":"confirmed|refuted|inconclusive"}`
  and `{"op":"retract","edge":<key>,"reason":...}` name an edge's key.
- A batch applies all or none; an error names the bad record. An edge on a
  bounded or direct site is rejected.
- After a rebuild the overlay refuses to load, naming both fingerprints. It
  also refuses one written by an rllvm-query that numbers call sites
  differently. To start again, move or delete the file, or name another
  with `--overlay` (MCP: `path`).
- A save or compaction refuses when another writer changed the file since
  it was opened; reopen it and record again.

Over MCP:

- `record_edges` takes them as `records`, attaching the overlay beside the
  catalog first if none is loaded. They are walked at once but stay in
  memory until `save_overlay`, the only tool that writes: it appends them
  and reports how many were `saved`.
- `load_overlay` attaches the default file, or `path` — required for a
  program loaded with `inventory`. It refuses to replace an overlay holding
  unsaved records unless `discard_pending` is true.
- `list_overlay` shows the edges, the sites they cover, and `pending`
  unsaved records.
- Unloading a catalog drops its overlay and reports how many unsaved records
  went with it; reloading a rebuilt catalog drops an overlay bound to the
  old build. Save before unloading, reloading, or ending the session;
  unsaved records are not written on exit.

On the command line, pipe them as JSON lines to `rllvm-query --catalog
catalog.json overlay record`, which saves at once; `--overlay PATH` names
another file. `overlay list` shows edges and the sites they cover; `overlay
compact` rewrites the file as the current edges.

Overlay edges are hypotheses, never proof: no answer uses them unless asked.
`reach`, `closure` and `slice` walk them with `include_overlay`
(`--include-overlay`), skipping refuted edges and any below `min_confidence`
(`--min-confidence low|medium|high`, default `low`). A file `--overlay` names
must exist for a walk to read it. Over MCP they need an overlay that
`load_overlay` or `record_edges` attached; without one, `include_overlay` is
an error, never a direct-only answer.

- Every step through one is `kind: agent` with its `confidence`,
  `provenance` and `verdict`; quote the provenance when reporting it.
  `uncertainty.agent_path_steps` counts them. Non-zero means the
  path is not proven: report it as a hypothesis, never as reachability.
- `closure` lists functions reached only through agent edges in
  `uncertainty.agent_reached`; `uncertainty.overlay` gives `covered_sites` of
  `unresolved_sites`.
- A `slice` counts its agent edges the same way. To check one edge in scope,
  `emit_module` writes the slice's definitions, cut out with the
  `llvm-extract` beside the configured `llvm-link`, as one small `.bc` to
  verify or re-read; the answer gains `emitted`. An alias comes with the
  function it stands for. It needs a program loaded with `load_catalog`,
  writes nothing for an empty slice, and refuses when a `static` function in
  a contributing module shares its name with a function another contributing
  module names. Same-named `static` globals the slice references become one
  external declaration.

## Is a vulnerable function present and reachable?

Ask per build: features and configurations change what is compiled in.

1. `defs` on the function. No result: it is not in this captured program.
2. `callers`. None: it is linked but has no captured direct caller; check
   `uses` for its address being taken before concluding anything.
3. `reach` from each entry point to the function: `main`, or the exported
   API — `ffi_exports` lists a Rust library's. A path is evidence; no path is
   not proof (see below).
4. `callees` on the entry point shows what it actually does on the way.

## Reading the answer

Report what the answer supports, and say what it does not:

- **`scope` is not coverage.** `scope` is what the catalog claims; `analysis`
  is what actually parsed. Report failed, missing, unsupported or unbuilt
  modules; an answer covers only what was analyzed.
- **No path is not unreachable.** An empty `reach` means no path over
  *resolved* edges. Mention `uncertainty.indirect_call_sites` and the
  frontier; say "no path found", never "unreachable".
- **`!callees` is an upper bound.** `llvm_target_bound` says an indirect call
  cannot target anything outside the set — not that it calls every member.
- **Address-taken lists are heuristics.** They appear in `indirect_targets`
  only when called with `heuristics: true`, and are never call edges.
- **Say how the name matched.** `resolution[].matched` is `mangled`,
  `demangled` or `fuzzy`; absent means the name matched nothing, which you
  report as no match, not silence. A fuzzy match is a guess to confirm with
  the user.
- **Know what a source digest proves.** `status_basis` is what `source_status`
  was checked against: `compiler` or `capture` dates from the build,
  `inventory` only from when the catalog was written. `source_status`:
  `modified` means locations may be off, `missing` means the file is gone,
  `unknown` means no digest was recorded to check.
- **Code without bitcode is invisible.** Assembly, prebuilt libraries and the
  Rust standard library contribute no functions; calls into them appear in
  `externals` as unbound. `indirect_targets`' `assumptions` state that
  `dlopen` and callbacks registered by uncaptured code escape its bound.
- **An FFI surface is only as complete as its attribution.** `ffi_exports`
  lists Rust definitions exported under an unmangled name, each attributed
  to Rust by debug info or by the module's producer.
  `uncertainty.functions_of_unknown_language` counts unmangled definitions
  it could not attribute and did not search — non-zero for code built
  without `-g`, or compiler-generated code such as a Rust binary's C
  `main`. Report it; a per-object catalog avoids it.
- **A field is evidence, not a target.** An indirect site's `via_field` and a
  use's `field` name the record field (`ops@8`: record and byte offset) the
  pointer is loaded from or stored into. No field means the IR proved none,
  not that there is none. `basis` says which IR evidence spoke; the member
  `name`, from debug info, is display only.
- **ODR copies are one definition.** A template or `inline` body emitted into
  many translation units is one function; plain `weak` copies may differ and
  stay ambiguous.
