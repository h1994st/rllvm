---
name: query
description: Choose the rllvm-query tool that answers a question about a captured program, and report its answer without over-claiming. Use for questions about definitions, callers, callees, reachability, function pointers or external calls in captured bitcode, and every time an rllvm-query answer is about to be reported.
---

# Querying captured bitcode

Load the program first with `load_catalog` or `inventory` (the `capture`
skill). With more than one catalog loaded, pass `catalog`.

## Choosing the query

| Question | Tool |
| --- | --- |
| Where is X defined, and in which configurations? | `defs` |
| What runs at `file:line`? | `at` |
| Who calls X? What does X call? | `callers`, `callees` |
| Where is X's address taken? | `uses` |
| What can this indirect call reach? | `indirect_targets` at the call site |
| Can A reach B — for example, is a vulnerable function reachable? | `reach`; if it finds no path, `closure` to see where the search stopped |
| Everything that reaches X, or that X reaches | `closure` with `direction` `in` or `out` |
| What does the program call outside itself? | `externals` |

A name can be the mangled symbol, the full demangled reading, or a bare
identifier.

## Reading the answer

Report what the answer supports, and say what it does not:

- **`scope` is not coverage.** `scope` is what the catalog claims; `analysis`
  is what actually parsed. Report failed, missing, unsupported or unbuilt
  modules; an answer covers only what was analysed.
- **No path is not unreachable.** An empty `reach` means no path over
  *resolved* edges. Mention `uncertainty.indirect_call_sites` and the
  frontier; say "no path found", never "unreachable".
- **`!callees` is an upper bound.** `llvm_target_bound` says an indirect call
  cannot target anything outside the set — not that it calls every member.
- **Address-taken lists are heuristics.** They appear only when heuristics were
  requested, and are never call edges.
- **Say how the name matched.** `resolution` reports `mangled`, `demangled` or
  `fuzzy`; a fuzzy match is a guess to confirm with the user.
- **Know what a source digest proves.** A `compiler` or `capture` digest dates
  from the build; an `inventory` digest only from when the catalog was written.
  `source_status: modified` means locations may be off.
- **ODR copies are one definition.** A template or `inline` body emitted into
  many translation units is one function; plain `weak` copies may differ and
  stay ambiguous.

Why these rules hold: `crates/query/README.md` in the rllvm repository.
