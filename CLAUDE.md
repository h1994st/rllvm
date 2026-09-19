# Engineering Guidance

## Scope

`rllvm-cc`, `rllvm-cxx`, and `rllvm-rustc` capture LLVM bitcode alongside normal
builds. `rllvm-get-bc` extracts it; `rllvm-info` inspects a module;
`rllvm-compdb` generates modules from `compile_commands.json`;
`rllvm-query` answers source-level questions about a catalog. Helpers:
`rllvm-init` and `rllvm-completions`.

`rules_rllvm` is a separate Bazel-only project that does not use these binaries.
Do not change anything here to serve it.

## How to work

**Reuse before you add.** Look for the existing helper, type, or test fixture
before writing a new one. Duplicated logic and parallel abstractions for the
same idea are the main way this codebase decays: two copies drift, and the
drifted one becomes a bug nobody sees. If something almost fits, extend it or
lift the shared part out — don't clone it. The same goes for dependencies:
prefer what is already in `Cargo.toml`.

Make the smallest coherent change that satisfies the request. Follow existing
patterns. Keep unrelated refactoring separate.

The workspace has three published crates. `rllvm-core` is the library;
`rllvm` holds the wrapper binaries; `rllvm-query` is the analysis library and
CLI, and the only crate that links LLVM. Put a new module in the crate that
owns its responsibility, and remember that anything `rllvm` or `rllvm-query`
reaches becomes public API of `rllvm-core`.

### Code

- Library code returns `Result` using the `thiserror` enum in `error.rs`; avoid
  exiting or panicking. Logging uses `tracing`.
- `constants.rs` is internal. Public items in `utils/` are public API; use
  `pub(crate)` for internal helpers.
- Do not define constants or lookup tables inside functions; lift them to module
  level.

### Changes

- Carry authorized work through the relevant checks and PR submission or update.
  Stop at PR handoff unless asked to monitor CI or continue, and do not request
  approval again for steps already authorized.
- One PR per issue. Use [Conventional Branch](https://conventionalbranch.org/)
  names, and separate worktrees only for concurrent independent work. Order
  dependent work and make stacked PR bases explicit.
- After a parent is squash-merged, rebase the dependent commits onto current
  `main` and then retarget. Retargeting alone leaves the parent's diff.
- Commits and PR titles use Conventional Commits. A subject is one idea under 60
  characters; a subject joining two changes with a comma is two commits. Branch
  commits carry **no body**: the squash body is built from subjects, so a body is
  restatement someone deletes by hand at merge. A `BREAKING CHANGE:` footer is
  machinery, not a body, and stays. Do not bump `version` by hand — releases
  derive from commit types, and below 1.0 `feat:`/`fix:` bump the patch while
  `feat!:` bumps the minor. See [RELEASING.md](RELEASING.md).
- A user-facing feature ships an example under `examples/<name>/`, and every
  example ships a `check.sh` that runs its documented flow and asserts the
  outcome. `crates/tools/tests/examples.rs` runs them all; exit 77 means a
  prerequisite is missing.

### Writing

- Issues and PRs state problem, cause, fix, verification, in plain language
  without conversational framing. Follow the templates.
- A PR body is 20 lines or fewer — count them before posting. One sentence each
  for problem and cause; one bullet per change and per check. Do not restate the
  fix under verification, and give a rejected option one clause rather than a
  paragraph. An issue may run longer, but only to describe the problem.
- Do not hard-wrap PR bodies, issue bodies or comments: one long line per
  paragraph, and let the renderer wrap. Markdown in the repository keeps its
  wrapping.
- `README.md` is the user-facing source of truth: keep it short and practical,
  and put rationale in issues or the code, not there. `site/build.py` generates
  `site/index.md`, which is never committed. Validate links and site generation
  when changing the README.
- `docs/` is gitignored except `docs/CATALOG.md`.
- Use repository-relative paths. Keep committed benchmark evidence to compact
  summaries; raw logs stay out of Git.

## Development and verification

Rust edition 2024, MSRV 1.88. LLVM/Clang is required: `brew install llvm` or
`apt install llvm llvm-dev clang libclang-dev`. Rust bitcode needs compatible
LLVM readers; `rustc -vV` reports its LLVM version.

```bash
cargo build
cargo test -p rllvm-core -p rllvm
cargo test parsing_lto  # one test by name
cargo clippy -p rllvm-core -p rllvm --all-targets -- -D warnings  # CI gate
cargo fmt --all --check  # CI gate
```

`rllvm-query` is its own crate and the only one that links LLVM. It is not in
`default-members`, so `cargo test` never builds `llvm-sys`; CI runs it as
`cargo test -p rllvm-query`. The gates above do not cover it, so run these when
touching it, after `cargo build` has produced `rllvm-compdb` for its tests to find:

```bash
cargo test -p rllvm-query
cargo clippy -p rllvm-query --all-targets -- -D warnings
```

Choose checks that validate the changed behavior; rerun affected checks after a
fix rather than broadening by default.

- Tests use isolated `RLLVM_CONFIG` files. Never depend on or modify the
  developer's home configuration.
- Name tests after the behavior, without a `test_` prefix. Confirm a regression
  test fails before its fix, at the layer that can actually break. Prefer
  behavioral assertions over file-existence checks.
- Shared integration fixtures live in the `rllvm-testkit` crate
  (`crates/testkit`).
- Do not move a built target directory: integration binaries embed paths.
- Benchmarks need idle, coordinated resources and recorded conditions. Parallel
  correctness runs are not benchmarks.

### Python utilities

Python 3.14+ handles repository utilities (benchmarks, site generation), managed
with `uv`. Run Python as `uv run python`, never bare `python`/`python3`. Add
dependencies with `uv add`, updating `pyproject.toml` and `uv.lock` together.
Typer for CLIs; pytest with plain assertions for tests. Mark real
compiler/build-system tests `pytest.mark.full` — they are excluded by default.

```bash
uv sync && uv run pytest
uv run ruff check . && uv run ruff format --check . && uv run ty check
uv run python site/build.py
```

## Contracts

These invariants hold across the codebase. The reasoning is in the code; what
follows is what must stay true.

### Compiler arguments

Every compiler-owned flag reaches the real compiler, including `-c`, `-v`,
`--help`, and `--version`. Wrapper options are long-only and prefixed
`--rllvm-`. Diagnostics go to stderr — build systems read compiler stdout.

Argument tables live in `constants.rs`. Arity controls consumption independently
of the handler, so a wrong arity silently swallows the next argument. Flags
needed in both phases, such as `-pthread` and `-arch`, must reach secondary
compilations and relinks.

### Response files and transport

`utils/response_file.rs` follows Clang's GNU UTF-8 syntax. A nonexistent `@`
name stays literal so linker values like `@rpath/...` survive. Check tokenizer
changes against real Clang.

Original argv goes to the real compiler; classification uses expanded arguments.
Generated commands use the shared transport helper for OS argument-size limits,
preserving `OsStr` bytes and inline empty arguments, which GNU response files
would discard.

### Recorded paths

Each object's dedicated section records a **newline-terminated** bitcode path,
and linkers concatenate these sections. Use `__RLLVM,__rllvm_bc` on Mach-O and
`.rllvm_bc` elsewhere — never LLVM's `.llvmbc`/`.llvmcmd`, which wasm-ld
discards. Keeping an unclaimed section is also what makes eBPF work unchanged.
Every Mach-O writer sets `no_dead_strip`. Prefer `llvm-objcopy` for embedding;
the `object`-crate rebuild can lose unmodelled load commands and invalidate
code signatures. A relocatable Mach-O object holds every section in one
unnamed segment, so `--add-section` names an empty segment and the section's
own `segname` is written afterwards: asking objcopy for a named segment makes
it append a second `LC_SEGMENT_64`, which `ld64` tolerates and `ld64.lld`
never reads.

Artifact identity derives from source, requested output, compiler, and settings,
so variants stay distinct in a shared `bitcode_store_path`. The public path hash
is stable. Every writer resolves `bitcode_root` the same way.

### Cache validity

A hit is valid only against freshly preprocessed input, current dependency
contents, command and compiler identity, working directory, and environment.
Generate uncached bitcode when inputs cannot be validated; side inputs absent
from preprocessing are out of scope.

Only the user's original compilation may write its dependency outputs. Publish
cache entries atomically, and never delete a preexisting file because its name
matches.

### LTO

Dispatch on object content, not just `-flto`: fat LTO produces a native object,
and needs the path in both halves. Full LTO provides a merged module; ThinLTO
does not. Queries, non-linking actions, and configure-only mode must not stage a
linker marker. Preserve user-requested linker temporaries.

### Rust

Make future bitcode paths absolute without canonicalizing files rustc has not
created yet. Linked crates carry paths through a marker; archive members are
patched after compilation. Never patch a finished Rust binary — it invalidates
the Darwin code signature. Metadata-only and procedural-macro invocations pass
through without capture.

### Queries

`extract.rs` is the only module with `unsafe` outside test code, and no LLVM
handle leaves it.

Answers never claim more than they know. `scope` is quoted from the catalog and
never shrinks; what was actually read is reported under `analysis`. `!callees`
is an upper bound, not a reachable set. The address-taken inventory is a
heuristic: opt-in, and never a graph edge. An empty `reach` is not
unreachability.

ODR copies of a symbol are one definition: C++ emits a template or `inline`
body into every translation unit that used it. Plain `weak` copies may differ
and stay ambiguous, and `available_externally` is never a binding candidate.

The mangled symbol is the identity, so demangled readings live in the envelope's
`symbols` table rather than beside each symbol. A name resolves exactly before
fuzzily, and the answer always says which.

A source digest records *when* it was taken, because that decides what a match
proves: a `compiler` or `capture` digest dates from the build, an `inventory`
one only from when the catalog was written.

MCP stdout carries protocol frames only.

## Current limitations

- Universal builds are unsupported.
- Human-readable binary inspection uses the first recorded module; `--json`
  inventories all of them. Whole-program inspection uses extracted `.bc`.
- Link mode performs repeated compilations (#51).
