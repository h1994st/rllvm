# Workflow benchmarks

This development harness compares ordinary builds with rllvm capture and
extraction on pinned C, C++, and Rust projects. It measures clean, unchanged
incremental, and single-source edited workflows. Correctness gates determine
whether a sample can enter performance summaries. The existing Criterion
microbenchmarks remain available through `cargo bench`.

## Environment and source preparation

Run commands from the rllvm repository root. The harness supports Linux and
macOS and requires Python 3.14 or newer, [uv](https://docs.astral.sh/uv/), Git,
matching Clang/LLVM tools, Rust/Cargo, CMake, and Ninja. Autotools profiles
also need Autoconf, Automake, libtool, and Make. The C++ profiles need
pkg-config and explicit libev, OpenSSL, and zlib prefixes; the Autotools C++
profile additionally needs c-ares. Rust's LLVM producer must match the LLVM
readers; compare `rustc -vV` with `llvm-config --version`.

```sh
uv sync --locked
cargo build --locked --release --bins -j2
uv run python -m benchmarks profiles
```

Choose a directory containing local source checkouts named `nghttp2`,
`nghttp3`, `ngtcp2`, and `quiche`. Only checkouts needed by the selected
profiles are required. Preparation clones private snapshots from their Git
objects, fetching missing pinned revisions when necessary. It checks out the
recorded submodule pins, runs Autotools bootstrap where needed, and performs
locked Cargo fetches. It leaves the supplied repositories, branches, dirty
files, and submodules unchanged. Preparation needs network access if the
required objects or Cargo packages are not already available. Timed Cargo
builds run offline.

All source pins live in [recipes.py](recipes.py):

| Project | Commit | Required submodules |
| --- | --- | --- |
| [nghttp2](https://github.com/nghttp2/nghttp2) | `a49ccc728863e7d8e5da369552e889e95399bd37` | `tests/munit`; C++ also `third-party/urlparse` |
| [nghttp3](https://github.com/nghttp3/nghttp3) | `f1e4328b9afd4982ee3e9fd822a605012e3d14b3` | `tests/munit`, `lib/sfparse` |
| [ngtcp2](https://github.com/ngtcp2/ngtcp2) | `72a85865dd3a4b33fe5830baf461ed50ebb7ad0e` | `tests/munit` |
| [Quiche](https://github.com/cloudflare/quiche) | `c8da372daa06b7cb51aa23b1a55bfe395dcf3d46` | None required by this profile |

Quiche uses the benchmark-owned [Cargo.lock](fixtures/quiche/Cargo.lock).
Preparation records its SHA256, source commit/tree, submodule revisions, tool
versions and hashes, dependency libraries and hashes, and complete commands.

The default profiles are `nghttp2-c-cmake`, `nghttp2-cxx-cmake`, and
`quiche-cargo`. Optional profiles are `nghttp2-c-autotools`,
`nghttp2-cxx-autotools`, `nghttp3-c-cmake`, `nghttp3-c-autotools`,
`ngtcp2-c-cmake`, and `ngtcp2-c-autotools`. Use repeated `--profile` options
for a subset, or `--all` for all nine. Output directories must be new.

Tool lookup uses only the explicitly supplied `--tool-root` directories,
checking both `ROOT/NAME` and `ROOT/bin/NAME`. Supply multiple roots when LLVM,
build tools, Rust proxies, and rllvm binaries are installed separately. Keep
executable aliases such as `clang++` intact. This example uses user-selected
build-tool and Rust directories:

```sh
EXAMPLES_ROOT=../rllvm-examples
BUILD_TOOLS_BIN=/path/to/build-tools/bin
RUST_TOOLS_BIN=/path/to/rust-proxies/bin
LLVM_BIN="$(llvm-config --bindir)"
BENCH_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rllvm-bench.XXXXXX")"
DEPENDENCIES_ROOT="$BENCH_ROOT/dependencies"
mkdir "$DEPENDENCIES_ROOT"
```

On Homebrew, the prefix directory names required by the harness differ from
some formula names. Create the following links in that scratch directory:

```sh
ln -s "$(brew --prefix libev)" "$DEPENDENCIES_ROOT/libev"
ln -s "$(brew --prefix openssl@3)" "$DEPENDENCIES_ROOT/openssl"
ln -s "$(brew --prefix zlib)" "$DEPENDENCIES_ROOT/zlib"
ln -s "$(brew --prefix c-ares)" "$DEPENDENCIES_ROOT/libcares"
```

On Linux, create the same named prefix layout for the dependency installations
you intend to compare. A dependency prefix must contain its headers and
libraries, and pkg-config metadata where applicable. An empty dependency root
is sufficient for the C-only and Quiche profiles. Resolved dependencies and
configuration parity are checked; silently omitting a dependency is not an
alternative profile.

```sh
uv run python -m benchmarks prepare \
  --examples-root "$EXAMPLES_ROOT" \
  --dependencies-root "$DEPENDENCIES_ROOT" \
  --tool-root "$LLVM_BIN" \
  --tool-root "$BUILD_TOOLS_BIN" \
  --tool-root "$RUST_TOOLS_BIN" \
  --tool-root target/release \
  --output "$BENCH_ROOT/prepared"
```

Add a root containing Git or pkg-config if neither is in `BUILD_TOOLS_BIN`. Use
`--profile nghttp2-c-cmake` here for a smaller preparation, or `--all` for all
profiles. Preparation prints the resolved `prepared-group.json` path.

## Planning, correctness, and measurements

A dry run records the runner's actual planned commands with null measurements.
It does not configure, compile, extract, edit sources, or execute preflight and
diagnostic commands. Input-dependent validation is explicitly marked planned.
It establishes no correctness or performance claim.

```sh
uv run python -m benchmarks run \
  --manifest "$BENCH_ROOT/prepared/prepared-group.json" \
  --dry-run --jobs 2 --output "$BENCH_ROOT/plan"

uv run python -m benchmarks run \
  --manifest "$BENCH_ROOT/prepared/prepared-group.json" \
  --smoke --jobs 2 --output "$BENCH_ROOT/smoke"
```

A smoke run has one repetition and is correctness evidence. Before publishing
measurements, stop other builds, tests, and diagnostic work on the host. Record
power/thermal conditions and any background activity; start/end load readings
alone do not establish an idle machine. The harness serializes its own runs
with a host lock. It does not lock out unrelated applications.

For a baseline, use a known committed harness and rllvm build, three
repetitions, and a fixed job count. Build rllvm before timing and preserve the
build command, revision, and binary hashes. Supplied tools may come from
another checkout, so the harness never infers their source revision from its
own Git HEAD. An optional provenance file explicitly states the known build
origin:

```sh
RLLVM_REVISION="$(git rev-parse HEAD)" \
PROVENANCE_PATH="$BENCH_ROOT/rllvm-provenance.json" uv run python - <<'PY'
import json
import os
from pathlib import Path

Path(os.environ["PROVENANCE_PATH"]).write_text(json.dumps({
    "schema_version": 1,
    "kind": "rllvm-provenance",
    "provenance": {
        "revision": os.environ["RLLVM_REVISION"],
        "build_command": "cargo build --locked --release --bins -j2",
    },
}) + "\n")
PY

uv run python -m benchmarks run \
  --manifest "$BENCH_ROOT/prepared/prepared-group.json" \
  --baseline --repetitions 3 --jobs 2 --seed 144 \
  --extraction-repeats 2 \
  --rllvm-provenance "$BENCH_ROOT/rllvm-provenance.json" \
  --output "$BENCH_ROOT/baseline"
```

Use that provenance example only when `target/release` was built from the
stated revision with that command. Save the harness revision and host
conditions with the retained run evidence. The manifest records a hashed
hostname, OS, architecture, CPU model/count, physical memory when available,
and load; missing hardware fields include reasons. Fresh preflight checks
reject changed tool bytes/versions, source commit/tree/tracked state, submodule
pins/tracked state, or the copied Cargo lock before measured work. A mismatch
requires preparing again and preserves invalid evidence instead of relabeling
the old identity.

## What is compared

Every repetition compares four arms:

| Arm | Private C/C++ bitcode cache before clean build |
| --- | --- |
| `native` | No rllvm capture |
| `wrapped-uncached` | Disabled |
| `wrapped-empty-cache` | Enabled and empty |
| `wrapped-primed-cache` | Enabled and primed with the same original inputs, argv, working directory, and environment |

Build/store/extraction paths are reset before each clean trial. Priming runs
outside timing and retains only its private cache. All arms then perform an
unchanged incremental build, followed by a deterministic version-string source
edit and an edited incremental build. The edit and restoration are hash checked
and recorded. Incremental trials retain build artifacts; Cargo's own artifact
cache is recorded separately from rllvm's C/C++ bitcode cache. Rust capture
does not gain a C/C++ cache merely because that arm enables one.

The seeded cyclic order balances all four positions over complete blocks of
four repetitions; the default three-repetition block is approximately balanced.
OS filesystem caches remain uncontrolled; the harness never claims a cold OS
cache or flushes it. Trials and phases do not change compiler-visible paths or
environment variables. Preflight checks only recorded identities and tracked
source state; it cannot detect later concurrent changes, untracked/generated
source edits, or changes to unrecorded compiler/runtime side inputs.

Native and wrapped arms use equivalent source/configuration work: C/C++ uses
`-O2 -g`, CMake `RelWithDebInfo` with `-DNDEBUG`, and explicit tools/dependency
prefixes. Cargo uses offline locked release builds, debug information, and
`codegen-units=1` for both target crates and host build dependencies. This
one-codegen-unit baseline is required for comparable native/wrapped work and is
not Cargo's unrestricted default. Optional project features are explicitly
selected by each recipe and checked against the resulting build evidence.

Each wrapped workflow extracts the selected artifact, extracts every declared
target, repeats the all-target extraction, and runs `rllvm-info` on the
selected module. Reports sum the sample's complete `timed:*` wall phases and
pair each wrapped sample with native in the same run/repetition. Clean,
unchanged, and edited states remain separate. Reports show raw samples,
median/range, paired median overhead, and phase costs; invalid or incomplete
samples never enter ratios. Configuration, build, extraction, and inspection
timing is distinct from priming, correctness validation, and diagnostic replay
cost.

## Correctness and coverage boundaries

Every target must preserve native behavior and project symbol definitions,
verify as LLVM IR, and cover independently evidenced project sources. Native
and wrapped library APIs must agree. Edited library IR must contain the actual
edited version string, rather than merely a changed hash. The selected target,
complete target set, and repeated extractions must agree semantically. A
successful build or a nonempty bitcode file alone does not satisfy these gates.

CMake codemodel/compile commands, Autotools verbose compile/link evidence, and
Cargo compiler-artifact/verbose rustc evidence establish source requirements.
Static archives include their project members; dynamically linked consumers
need not contain shared-library bodies. nghttp2 C++ also checks `nghttp` and
`h2load`; Quiche checks its static library, shared library, and client.

Coverage is project scoped. External shared libraries, prebuilt runtimes/Rust
standard libraries, assembly, Cargo dependency crate bodies, and build-script
native inputs have explicit exclusions. The captured IR consists of
translation/crate units merged by `llvm-link`, without whole-program LTO.
Neither these exclusions nor matching project symbols imply that all dependency
code or every runtime path is captured.

Resource observations come from the waited command and reaped descendants. CPU
usage and maximum observed command RSS are not a simultaneous process-tree
memory peak. Disk evidence separates logical/allocated bytes for build
artifacts, bitcode store, private cache, and extracted modules. Counts come
from separate diagnostic replays with compiler/rustc/link/archive exec probes;
they exclude hidden subprocesses and threads. Missing query, preprocessing,
cache-hit, or bitcode-compilation observations carry reasons rather than
invented zeros. Diagnostic health is required for valid samples, and all replay
executions remain in raw evidence.

## Retaining evidence and offline reports

```sh
uv run python -m benchmarks report \
  "$BENCH_ROOT/baseline/run-group.json" \
  --output "$BENCH_ROOT/report"
```

Reporting reads saved records only and writes `report.md`, `samples.csv`, and
`report.json`. Multiple run paths can be supplied for a compatible repeated
comparison. Changed source/tool/dependency/configuration or stable host
identity cannot be pooled. Reports reject malformed, truncated, contradictory,
or missing required evidence and refuse to overwrite existing report artifacts.

Keep the run's `run.json`, five indexed JSONL streams, preparation manifests,
provenance, and every referenced log/build evidence file together under a
stable run identifier. Record paths can be absolute; relocating or packaging
evidence requires preserving their relationships and documenting the original
layout. Compact baseline metadata/reports can be committed under
`benchmarks/baselines/`; large logs and build directories stay outside Git. Do
not discard failed observations when retaining results.

Completed immutable validation logs are deduplicated in the owned run directory
through a private content-addressed hard-link pool. Every original pathname and
byte remains available, and every validation command still executes. Storage
operations run outside timed commands and record duration, avoided allocated
bytes, and any unavailable reason. Unsupported hard links preserve originals.
Do not modify completed logs or the pool. Keep enough disk for unique IR and
build artifacts; archival compression, when needed, must finish between serial
measurements and must preserve evidence. Storage savings do not count as
improved rllvm workflow timing.

## Recorded baseline

The [2026-09-12 Apple M4 baseline](baselines/2026-09-12-apple-m4/README.md)
contains compact results from 108 validated samples across the three default
profiles, with build-only and complete-workflow comparisons and provenance.
Raw archives and verbose reports remain outside Git. Host activity, toolchain,
cache conditions, and coverage limits are recorded with the results. These are
observations, not a CI performance threshold.

## Extending and checking the harness

Add a pinned `Recipe` in [recipes.py](recipes.py), declare all required tools,
dependencies, submodules, targets and build settings, and define a
deterministic source edit plus native behavior/API probes. Update an
independent coverage provider when the build system cannot prove the required
project sources. For Cargo, retain a benchmark-owned lock. Add small real
subprocess regressions that fail before the change, then validate the new
profile's real native and wrapped artifacts before measuring it. Document its
coverage exclusions and keep source preparation separate from measured builds.

```sh
uv run ruff format --check
uv run ruff check
uv run ty check
uv run pytest
uv lock --check
uv run python site/build.py
```

Tests default to existing `target/release` rllvm binaries. To reuse a debug
build, set `RLLVM_BENCH_TEST_BIN_DIR=target/debug`. The Linux/macOS CI Build
and Test jobs use that override and run small LLVM-backed correctness fixtures
with bounded jobs; CI has no timing thresholds or published performance ratios.
