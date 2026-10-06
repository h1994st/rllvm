---
description: Resolve unresolved indirect calls into a labeled call-graph overlay
argument-hint: [catalog.json]
---

Use the complete-callgraph skill on the catalog `$ARGUMENTS`, or on the only
loaded catalog when no argument is given. Load it if it is not loaded.

Resolve the unresolved indirect calls that matter to the question at hand,
save the overlay, and report:

- a table of fields to recorded edges: field, target, confidence, and the
  provenance quoted;
- coverage: `covered_sites` of `unresolved_sites`, and what stays unresolved;
- any path found through the overlay, labeled "not proven", with a proven
  path if one exists.
