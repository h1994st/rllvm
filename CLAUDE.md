# Engineering Guidance

## Scope and workflow

`rllvm-cc`, `rllvm-cxx`, and `rllvm-rustc` capture LLVM bitcode alongside normal
builds. `rllvm-get-bc` extracts it; `rllvm-info` inspects a module. Helpers:
`rllvm-init` and `rllvm-completions`.

`rules_rllvm` is a separate Bazel-only project that does not use these binaries.
Do not change anything here to serve it.

Make the smallest coherent change that satisfies the request. Follow existing
patterns; add abstractions or dependencies only when needed for the requested
behavior. Keep unrelated refactoring separate.

- Carry authorized implementation work through relevant checks and PR submission
  or update. Stop at PR handoff unless asked to monitor CI or continue.
- For CI repairs, confirm the affected CI check passes. For analysis and review,
  report findings unless changes were requested.
- Do not request approval again for steps already authorized.
- A single ongoing job does not require a worktree. Use separate worktrees for
  concurrent independent issue work. Follow
  [Conventional Branch](https://conventionalbranch.org/) for task branches.
  Keep each issue reviewable in its own PR. Order dependent work and make stacked
  PR bases explicit.
- After a parent is squash-merged, rebase only the dependent commits onto current
  `main`, then retarget the PR. Retargeting alone can leave the parent's diff.
- Clean up task-owned temporary files and merged worktrees when requested,
  preserving uncommitted user work.

## Development and verification

Rust edition 2024, MSRV 1.88. LLVM/Clang is required: `brew install llvm` or
`apt install llvm llvm-dev clang libclang-dev`. Rust bitcode needs compatible
LLVM readers; `rustc -vV` reports its LLVM version.

Choose checks that validate the changed behavior. Reuse recorded passing results
when relevant code and test conditions are unchanged. After a fix, rerun affected
checks; broaden testing only for a failure or unresolved concern.

```bash
cargo build
cargo test --all
cargo test parsing_lto                    # one test by name
cargo clippy --all-targets -- -D warnings  # CI gate
cargo fmt --all --check                   # CI gate
```

- Integration tests use the `rllvm()` helper and isolated `RLLVM_CONFIG` files.
  Manual wrapper checks also use a scratch `RLLVM_CONFIG`; never change or depend
  on the developer's home configuration. Use temporary sources and out-of-tree
  builds when checking another repository. Put `--rllvm-verbose=3` before compiler
  arguments to log subcommands.
- Name Rust tests after behavior, without a `test_` prefix. Confirm new regressions
  fail before their fix at the layer that can break. Prefer native and extracted
  behavior checks over file-existence assertions.
- Parallel workers use bounded job counts and separate Cargo target directories.
  Do not move a built target directory: integration binaries embed executable paths.
- Performance measurements require coordinated, otherwise idle resources and
  recorded build/cache conditions. Parallel correctness runs are not benchmarks.

### Python utilities

Python 3.14+ is for repository utilities, including benchmarks and site generation.
Use `uv` to manage this utility-script project:

- Add dependencies with `uv add` or `uv add --dev`, updating `pyproject.toml` and
  `uv.lock` together. Run Python with `uv run python`, never bare `python` or `python3`.
- Use Typer for CLIs and pytest fixtures and plain assertions for tests.
- Default tests focus on basic correctness. Mark real compiler, build-system,
  and complete workflow tests with `pytest.mark.full`; they are excluded by default
  and selected in CI only for release-please PRs. Use test doubles when only an
  external tool's data matters.
- Ruff and ty configuration lives in `pyproject.toml`.

```bash
uv sync
uv run pytest
uv run pytest -m full  # opt-in compiler/build-system integration tests
uv run ruff check .
uv run ruff format --check .
uv run ty check
uv run python site/build.py
```

## Compiler contracts

Paths below are relative to `src/`. Preserve these contracts; current limitations
listed afterward may change through work explicitly scoped to address them.

### Compiler arguments

Every compiler-owned flag reaches the real compiler, including `-c`, `-v`,
`--help`, and `--version`. Wrapper options are long-only and prefixed `--rllvm-`.
Diagnostics go to stderr; build systems depend on compiler stdout for queries,
identification, and preprocessing.

`arg_parser.rs` uses the tables in `constants.rs` to separate compile and link
arguments. Arity controls consumption independently of the handler: a wrong arity
swallows the next argument. Flags needed in both phases, such as `-pthread` and
`-arch`, must reach secondary compilations and relinks. Recognize `-oFILE` before
filename patterns while preserving both forms of `-object-file-name`.
`is_object_file()` returns `Ok(false)` for unrecognized arguments.

### Response files and command transport

`utils/response_file.rs` follows Clang's GNU UTF-8 response syntax. Nested paths
resolve from the process working directory; nonexistent `@` names remain literal
so linker values such as `@rpath/...` survive. Repeated references are not cycles.
Check tokenizer changes against real Clang, including quotes, escapes, BOMs, and
whitespace.

`CompilerArgsInfo::input_args()` retains original argv for the real compiler;
classification and internal consumers use expanded arguments. Save-temps ownership
must inspect `expanded_args()`.

Generated commands and marker compilations use the shared transport helper for
OS argument-size limits. Preserve `OsStr` bytes and direct empty arguments: GNU
response files discard quoted empties, so keep them inline between response-file
segments. Temporary files must outlive the child.

### Artifact identity and recorded paths

`arg_parser.rs` and `utils/path_utils.rs` derive C/C++ artifacts from the source,
requested output, compiler, and settings. Keep variants distinct even in a shared
`bitcode_store_path`. Wrapper constructors and builders retain the actual compiler
so public `args().artifact_filepaths()` queries match generated paths. Keep the
public path hash stable.

Each object's dedicated section records a **newline-terminated** bitcode path;
linkers concatenate these sections. Every writer uses the same `bitcode_root`
resolution.

Use `__RLLVM,__rllvm_bc` on Mach-O and `.rllvm_bc` elsewhere. Do not rename them
to LLVM's `.llvmbc` or `.llvmcmd`, which wasm-ld discards. Keeping an unclaimed
section also supports eBPF without special handling: libbpf skips it on load and
copies it through linking. Every Mach-O writer sets `no_dead_strip`; preserve
the user's dead-stripping flags.

Prefer `llvm-objcopy` for embedding: the `object`-crate rebuild can lose unmodelled
load commands.

### Cache validity and file ownership

`cache.rs` validates hits against fresh preprocessed input, current dependency
contents, command/compiler identity, working directory, and environment. A prior
depfile misses newly selected headers and changed `__has_include` results.
Generate uncached bitcode when current inputs cannot be validated. Side inputs
absent from preprocessing/dependencies are outside this cache's scope.

Only the user's original compilation may write its dependency outputs. Use
`without_dependency_flags()` for secondary compilations and markers; cache
validation uses private output and dependency files.

Publish cache entries atomically. Partial merges own a unique temporary directory.
Archive extraction builds a fresh archive beside the destination and replaces it
only after success: `llvm-ar rs` against an existing output retains stale members.
Cleanup must never remove preexisting files merely because their names match.

### LTO

Dispatch on object content, not just `-flto`: fat LTO produces a native object.
For bitcode objects, `lto.rs` and `compiler_wrapper/llvm/lto_marker.rs` record paths
through module assembly. Keep the `.ascii` newline and two-layer C/assembler
escaping. A `used` global introduces NUL termination and allocation/section-merging
problems. Fat objects need the path in both their native and bitcode halves.

Save-temps eligibility includes combined source/link invocations. Queries,
non-linking actions, and configure-only mode must not stage a linker marker.
Build markers for the requested target/language, then reset `-x` to `none` before
appending the marker object after the user's inputs.

Preserve user-requested linker temporaries, including the selected module: copy
it to the retained rllvm path rather than renaming it away. Full LTO provides a
merged module; ThinLTO does not. Mixed marker/save-temps inputs require the existing
diagnostic rather than silently merging a translation unit twice.

### Rust and inspection

`compiler_wrapper/llvm/rustc_args.rs` handles explicit `-o` and Cargo's `--out-dir`,
crate name, and extra filename. Make future bitcode paths absolute without
canonicalizing files rustc has not created yet. Linked crates carry paths through
a marker; archive members are patched after compilation. Never patch a finished
Rust binary: doing so invalidates its Darwin code signature.

Cargo supplies the real compiler path in `RUSTC_WRAPPER` mode. Configured rustc and
`RLLVM_REAL_RUSTC` selection apply to direct invocation. Metadata-only and
procedural-macro invocations pass through without capture.

Test `rllvm-info` against real `llvm-dis` output as well as literal fixtures.
Basic-block labels can have quoted names and trailing predecessor comments; the
entry block can be implicit.

## Current limitations

- Universal builds are unsupported.
- Binary inspection uses only the first recorded module, when available;
  whole-program inspection uses the extracted `.bc`.
- Link mode performs repeated compilations (#51). Changing this is a separate
  behavior/performance task.

## Conventions and documentation

- Library code returns `Result` using the `thiserror` enum in `error.rs`; avoid
  exiting or panicking. Logging uses `tracing`.
- `constants.rs` is internal. Public items in `utils/` are public API; use
  `pub(crate)` for internal helpers.
- The `docs/` directory is intentionally excluded.
- Published documentation, issues, and PRs use repository-relative paths or generic
  placeholders. Limit committed benchmark evidence to compact summaries and
  provenance; keep raw logs and build artifacts outside Git.
- `README.md` is the user-facing source of truth. `site/build.py` generates
  `site/index.md`; do not edit or commit that page. Validate links and site
  generation when changing the README.

Issues, PRs, and comments use the project's voice: problem, cause, fix,
verification. Follow issue and PR templates and omit conversational framing.

Commits and PR titles use Conventional Commits (`fix:`, `feat:`, `docs:`, etc.).
Keep commit bodies short, and leave them empty in most cases. Do not bump `version`
by hand; releases derive from commit types. Below 1.0, `feat:`/`fix:` bump the patch
and `feat!:` bumps the minor. Mark breaking changes. Pushing a tag does not trigger
a release; see [RELEASING.md](RELEASING.md).
