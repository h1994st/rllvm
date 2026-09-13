# Module catalog format

`rllvm-info --json` inventories modules without compiling or merging them.
`rllvm-get-bc --output-dir DIR` copies selected modules and writes `catalog.json`
in a new directory. The compilation database importer uses the same format for
planned, generated, unsupported, and failed compilation entries.

## Version and scope

Version 1 has `schema_version: 1` and `kind: "rllvm-module-catalog"`.
Readers reject unsupported versions, duplicate/empty module IDs, inconsistent
entry counts, and available entries without a path and content hash.

| Field | Meaning |
| --- | --- |
| `origin.kind` | Evidence producer, such as an artifact or compilation database. |
| `origin.input`, `origin.sha256` | Original input and its content hash, when known. |
| `scope.kind` | Recorded module inventory or selected analysis compilations. |
| `scope.total_entries`, `scope.selected_entries` | Known entries before and after selection. |
| `scope.selection` | Requested module IDs, source paths, and configuration IDs. |
| `scope.selection_history` | Earlier filters preserved when a selected catalog is narrowed again. |
| `scope.analysis_arguments` | Explicit analysis overrides, including runs where all entries fail. |
| `scope.whole_program_complete` | `null` when program completeness is not established, as with these producers. |
| `scope.limitations` | Unrecorded inputs, unreadable archive boundaries, and unavailable provenance. |
| `modules` | Selected entries, including unavailable entries. |

Counts describe known entries, not the complete source/dependency closure.
An archive with unrecorded objects can still have readable recorded modules.
Traversal errors retain preceding evidence and failed entries; thin archives
are rejected explicitly. Inventory/copy operations never invoke a merge.

## Module entries

| Field | Meaning |
| --- | --- |
| `id` | Opaque entry identity; preserved when modules are copied or relocated. |
| `path` | Module file, or containing archive when `archive_member` is present. |
| `recorded_path` | Original path from a native recording section, if applicable. |
| `archive_member` | Archive member index and name; indexes distinguish duplicate names. |
| `content_sha256` | SHA-256 of the module bytes, separate from its identity. |
| `sources` | Known source associations, each with a path, optional directory, origin, and optional current-file hash. |
| `target_triple`, `data_layout` | Metadata read from the module's LLVM IR. |
| `compiler` | Recorded compiler path, resolved path, version, and hash when available. |
| `configuration_id` | Identity of a known recorded compilation configuration. |
| `build_identity`, `source_snapshot` | Recorded historical provenance, or `null`. |
| `ir_stage` | Known stage of IR generation, or `null`. |
| `debug_info` | Whether inspected IR has compile-unit debug metadata; `null` when uninspected. |
| `compilation` | Optional database occurrence, cwd, recorded output/argv, effective analysis argv, analysis ID, overrides, and analysis environment. |
| `status` | `planned`, `available`, `missing`, `unsupported`, or `failed`. |
| `diagnostic_path`, `diagnostics` | Optional diagnostic file and explanatory messages. |
| `unavailable_metadata` | Additional descriptions of metadata the producer cannot establish. |

Unavailable metadata is represented with `null` or empty association lists. In
particular, a source name or compiler string found in debug information does
not establish a source snapshot or a complete compiler/configuration identity.
Legacy path sections do not record those identities or an IR stage.

Different compilations of one source remain separate even if their module bytes
are identical. Consumers should use IDs for selection and hashes for integrity;
they should not derive IDs from filenames or assume a content hash identifies a
compilation configuration. Analysis overrides have their own analysis identity.

## Selection and relocation

Selections combine alternatives within an option and intersect different
options. An unmatched selector or empty intersection is an error. Sources match
known associations; relative CLI paths resolve from the caller's directory when
the association has a known absolute location. A relative legacy source with no
recorded directory can only be matched by its recorded spelling.

Catalog-relative module and diagnostic paths resolve from the catalog's
directory, independently of the current working directory. Copied outputs use
relative paths and preserve IDs, hashes, recorded provenance, and input-directory
groups for partial merging. Reusing a catalog without new selectors retains its
selection; further selections retain the earlier filters as history. Original
source/compiler/origin paths remain provenance; they are not rewritten to imply
the source tree or toolchain moved with the modules.

Copying refuses existing output directories and unavailable selected entries.
Each copied module is checked against its recorded content hash before
publication; the catalog is published last, atomically. An interrupted or failed
copy can leave partial files, but no completed catalog is published for that run.
Unselected missing modules do not prevent copying an available selection.

Explicit merged output requires compatible known target/data-layout metadata.
Archive output can retain modules with different targets. A legacy text path
manifest cannot represent embedded bitcode archive members; use the portable
catalog output for those inputs.
