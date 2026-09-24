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

## Choosing the query

| Question | Tool |
| --- | --- |
| Where is X defined, and in which configurations? | `defs` |
| What runs at `file:line`? | `at` |
| Who calls X? What does X call? | `callers`, `callees` |
| Where is X's address taken? | `uses` |
| Who can call X through a pointer? | `uses` for address-taken sites, then `indirect_targets` at each |
| What can this indirect call reach? | `indirect_targets` at the call site (`heuristics: true` adds the address-taken inventory) |
| Can A reach B — for example, is a vulnerable function reachable? | `reach`; if it finds no path, `closure` to see where the search stopped |
| Everything that reaches X, or that X reaches | `closure` with `direction` `in` or `out` |
| What does the program call outside itself? | `externals` |
| Which Rust functions can C call — the FFI surface? | `ffi_exports` |

A name can be the mangled symbol, the full demangled reading, or a bare
identifier.

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
  it could not attribute and did not search — non-zero for a module merged
  from C and Rust without `-g`. Report it; a per-object catalog avoids it.
- **ODR copies are one definition.** A template or `inline` body emitted into
  many translation units is one function; plain `weak` copies may differ and
  stay ambiguous.
