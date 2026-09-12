# Apple M4 baseline, 2026-09-12

All 108 samples passed: three repetitions of four treatments and three build
states for nghttp2 C, nghttp2 C++, and Quiche. The six optional profiles also
passed separate correctness checks. Those earlier correctness runs are not
performance baselines.

## Conditions

- rllvm and harness revision: `0b72a086185b3d4b55e4e5ea49b337a0db9a345f`.
  Release binaries were verified by `cargo build --locked --release --bins -j2`;
  their hashes and tool versions are in [provenance.json](provenance.json).
- Apple M4, 10 logical/physical cores, 24 GiB RAM, macOS 26.6.2, AC power.
- LLVM/Clang 22.1.8; rustc 1.98.0 using LLVM 22.1.8.
- Eight build jobs, three repetitions, seed 144, and two repeated extraction
  passes. Profiles ran serially after every other agent build, test, and
  diagnostic job stopped. Desktop background activity remained present and was
  recorded. This was not a dedicated idle host.
- OS filesystem cache was uncontrolled. Each treatment's native/build outputs,
  captured outputs, and private rllvm cache followed the recorded reset schedule.
  Priming used the original source; its cost is reported separately.
- Cargo used one codegen unit for both target and host release work, with
  `CARGO_INCREMENTAL=0`, package `quiche`, default features plus `ffi`, and the
  checked-in frozen dependency lock. This keeps native and wrapped compiler work
  comparable under the current capture behavior.

These are observations for the recorded fixtures and conditions, not performance
thresholds or a claim that a cache helps every workload. Compact timing samples,
ranges, paired ratios, diagnostic counts, and coverage are in
[summary.json](summary.json). Full per-command reports remain outside Git.

## Clean build only

Cells show median wall seconds and the median of the three paired ratios against
native. Configuration, extraction, inspection, priming, validation, and diagnostics
are excluded from this table. The raw build figures for all three states are in
[summary.json](summary.json).

| Profile | Native | Wrapped, cache off | Wrapped, empty cache | Wrapped, primed cache |
|---|---:|---:|---:|---:|
| nghttp2-c-cmake | 1.807 s | 3.327 s (1.84×) | 3.889 s (2.11×) | 2.878 s (1.61×) |
| nghttp2-cxx-cmake | 8.553 s | 15.635 s (1.83×) | 18.782 s (1.98×) | 10.244 s (1.20×) |
| quiche-cargo | 39.060 s | 65.785 s (1.59×) | 68.626 s (1.66×) | 61.477 s (1.50×) |

## Complete timed clean workflow

These totals include configuration where applicable and the build. Wrapped
samples additionally perform selected-target extraction, all-target extraction,
two repeated all-target extraction passes, and inspection of the selected merged
module. That is ten extraction commands per C/Cargo sample and sixteen per C++
sample. Priming, validation, and diagnostic work remain separate.

| Profile | Native | Wrapped, cache off | Wrapped, empty cache | Wrapped, primed cache |
|---|---:|---:|---:|---:|
| nghttp2-c-cmake | 5.908 s | 10.861 s (1.85×) | 12.293 s (2.08×) | 11.178 s (1.92×) |
| nghttp2-cxx-cmake | 14.252 s | 27.679 s (1.94×) | 31.549 s (2.08×) | 22.965 s (1.61×) |
| quiche-cargo | 39.060 s | 82.372 s (2.01×) | 85.203 s (2.06×) | 78.025 s (1.97×) |

An unchanged native build is usually very short, while the wrapped workflow still
performs the requested analysis operations. Its large workflow ratio should not
be read as wrapper-only overhead for a no-op build.

## Validated coverage

Module counts describe the observed extracted inputs. Definition counts are the
project-owned native definitions checked against both wrapped native outputs and
extracted IR. Dependencies, prebuilt runtimes, assembly, and dynamically linked
library bodies retain the boundaries documented by each recipe; these counts do
not claim complete source coverage for every dependency.

| Profile | Target | Modules | Project definitions |
|---|---|---:|---:|
| nghttp2-c-cmake | shared | 26 | 182 |
| nghttp2-c-cmake | static | 26 | 414 |
| nghttp2-c-cmake | tests | 39 | 382 |
| nghttp2-cxx-cmake | h2load | 12 | 235 |
| nghttp2-cxx-cmake | nghttp | 13 | 224 |
| nghttp2-cxx-cmake | shared | 26 | 182 |
| nghttp2-cxx-cmake | static | 26 | 414 |
| nghttp2-cxx-cmake | tests | 39 | 382 |
| quiche-cargo | client | 229 | 205 |
| quiche-cargo | shared | 215 | 170 |
| quiche-cargo | static | 278 | 472 |

The controlled source edit changes the library's returned version string to add
`-rllvm-benchmark` in a private source snapshot. Native API checks and extracted
LLVM string-constant checks require the changed value; the source is restored
afterward. The [original example checkouts](source-preservation.json) remained
clean at their pinned commits.

## Evidence and offline reproduction

- The externally retained `records.tar.xz` preserves the original run manifests and every
  indexed JSONL stream, including commands, measurements, operations, validations,
  diagnostics, and final samples. It also contains recorded build provenance,
  fixture identity, and host context. [evidence-manifest.json](evidence-manifest.json)
  lists SHA256 hashes and byte lengths of the original files.
- The externally retained `report.md`, `samples.csv`, and `report.json` were
  generated solely from unpacked saved records. Regeneration from the original
  run directories produced identical files.
- The externally retained `optional-correctness-records.tar.gz`
  preserves the six optional CMake/Autotools correctness results and commands.
  Its internal manifest hashes all 21 original records. No performance claim
  uses those measurements.
- Full build trees, bitcode, tool stdout/stderr, and diagnostic receipts remain
  outside Git. Their recorded paths and hashes identify retained local evidence;
  rerun the workflow to recreate these large artifacts elsewhere.

The repository contains compact results and provenance. Full raw records and
verbose reports are retained separately. Recreate the full record set by repeating
the measurements, or supply a retained archive to the offline command below.

The main archive is 23,224,488 bytes; its SHA256 is
`bfb736274ec8790b624c746e284630f5e5a7eafffdaf69abaea784ed5b5ddefd`.

Given the retained archive, regenerate the full report without building anything:

```bash
records_dir=$(mktemp -d)
tar -xJf "$RECORDS_ARCHIVE" -C "$records_dir"
uv run python -m benchmarks report "$records_dir/runs.json" \
  --output "$records_dir/regenerated"
```

The archive's `runs.json` uses relative manifest references. Original group
manifests and command records preserve their observed runtime paths.

## Repeating measurements

Use the recorded revision and tool versions, and configure the explicit source,
dependency, and tool roots described in the [benchmark guide](../../README.md).
The original preparation selected all nine profiles; only the three below were
used for this baseline. Build provenance must describe the binaries actually used.

```bash
uv run python -m benchmarks prepare --all \
  --examples-root "$EXAMPLES_ROOT" \
  --dependencies-root "$DEPENDENCIES_ROOT" \
  --tool-root "$RLLVM_BIN" --tool-root "$LLVM_BIN" \
  --tool-root "$RUST_BIN" --tool-root "$BUILD_TOOLS_BIN" \
  --tool-root "$SYSTEM_BIN" --output "$PREPARED"

for profile in nghttp2-c-cmake nghttp2-cxx-cmake quiche-cargo; do
  uv run python -m benchmarks run \
    --manifest "$PREPARED/prepared-group.json" --profile "$profile" \
    --baseline --repetitions 3 --jobs 8 --seed 144 --extraction-repeats 2 \
    --rllvm-provenance "$PROVENANCE" --output "$RUN_ROOT/$profile"
done
```

The full source, submodule, lockfile, flags, tool hashes, dependency versions,
and effective commands are retained per run. Source pins are:

| Project | Commit |
|---|---|
| nghttp2 | `a49ccc728863e7d8e5da369552e889e95399bd37` |
| nghttp3 | `f1e4328b9afd4982ee3e9fd822a605012e3d14b3` |
| ngtcp2 | `72a85865dd3a4b33fe5830baf461ed50ebb7ad0e` |
| quiche | `c8da372daa06b7cb51aa23b1a55bfe395dcf3d46` |
