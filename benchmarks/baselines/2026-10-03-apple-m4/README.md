# Apple M4 baseline, 2026-10-03

All 144 samples passed: 4 repetitions of 4 treatments and 3 build states for
nghttp2-c-cmake, nghttp2-cxx-cmake, and quiche-cargo.

## Conditions

- rllvm revision: `13366a0766657d166c4e0e585b729ec61a4ba75c`, built with `cargo
  build --locked --release --bins -j2`. Binary hashes and tool versions are in
  [provenance.json](provenance.json).
- Apple M4, 10 logical CPUs, 16 GiB RAM, `macOS-26.5.1-arm64-arm-64bit-Mach-O`.
- Compilers: `Homebrew clang version 23.1.2`; `rustc 1.98.1 (48a229cea
  2026-09-01)` using LLVM 22.1.8.
- 8 build jobs, 4 repetitions, seed 144, and 2 repeated extraction passes.
- Profiles ran serially from `2026-10-03T09:11:45.621600+00:00` to
  `2026-10-03T11:16:04.634851+00:00`. Recorded 1, 5 and 15 minute load averages
  at each profile's start and end: nghttp2-c-cmake 3.13/5.37/5.03 to
  4.85/4.94/4.96; nghttp2-cxx-cmake 4.85/4.94/4.96 to 4.60/4.13/4.45;
  quiche-cargo 4.60/4.13/4.45 to 4.95/5.36/5.45.
- Recorded limitations: OS filesystem cache is uncontrolled; no cache flush
  performed; CPU/RSS comes from waited command and reaped descendants; not
  simultaneous tree peak; private C/C++ bitcode cache and Cargo artifact cache
  are separate states; Cargo target and host release codegen-units=1; Cargo
  target and host release work uses one codegen unit; Prebuilt Rust standard
  libraries and assembly may be uncaptured.

Host conditions supplied when packaging, which the records cannot establish:

- Development host, not dedicated to benchmarking; desktop and other background
  activity was not stopped.
- Load averages (1, 5, 15 minutes) from `uptime`: 3.13 5.37 5.03 at the start
  and 4.95 5.36 5.45 at the end.
- AC power.
- 4 repetitions, 8 jobs, seed 144, and 2 extraction repeats.
- rllvm 13366a0; LLVM 23.1.2; rustc 1.98.1 (LLVM 22.1.8).

These are observations for the recorded fixtures and conditions, not performance
thresholds or a claim that a cache helps every workload. Compact timing samples,
ranges, paired ratios, diagnostic counts, and coverage are in
[summary.json](summary.json). Full per-command reports remain outside Git.

## Clean build only

Cells show median wall seconds and the median of the paired ratios against
native in the same repetition. Build time is the sum of each sample's
`timed:*-build` phases; configuration, extraction, inspection, priming,
validation, and diagnostics are excluded. The raw build figures for all three
states are in [summary.json](summary.json).

| Profile | Native | Wrapped, cache off | Wrapped, empty cache | Wrapped, primed cache |
|---|---:|---:|---:|---:|
| nghttp2-c-cmake | 1.226 s | 2.277 s (1.86×) | 2.693 s (2.20×) | 2.021 s (1.65×) |
| nghttp2-cxx-cmake | 4.760 s | 8.930 s (1.88×) | 9.680 s (2.04×) | 6.092 s (1.28×) |
| quiche-cargo | 29.305 s | 37.550 s (1.28×) | 39.548 s (1.35×) | 36.041 s (1.23×) |

## Complete timed clean workflow

These totals sum every timed phase of a sample. Native samples time
`timed:clean-build` and `timed:configure`. Wrapped samples additionally time
`timed:extract-all`, `timed:extract-repeat-0`, `timed:extract-repeat-1`,
`timed:extract-selected`, and `timed:inspect-merged-module`. Priming,
validation, and diagnostic work remain separate.

| Profile | Native | Wrapped, cache off | Wrapped, empty cache | Wrapped, primed cache |
|---|---:|---:|---:|---:|
| nghttp2-c-cmake | 5.499 s | 9.824 s (1.78×) | 10.936 s (1.98×) | 10.222 s (1.85×) |
| nghttp2-cxx-cmake | 10.368 s | 20.253 s (1.95×) | 21.758 s (2.10×) | 18.132 s (1.74×) |
| quiche-cargo | 29.305 s | 49.772 s (1.70×) | 51.739 s (1.77×) | 48.324 s (1.65×) |

Unchanged and edited states are in [summary.json](summary.json). An unchanged
native build is usually very short, while the wrapped workflow still performs
the requested analysis operations. Its large workflow ratio should not be read
as wrapper-only overhead for a no-op build.

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

The controlled source edit changes `lib/nghttp2_version.c` in nghttp2 to add
`-rllvm-benchmark` and `quiche/src/ffi.rs` in quiche to add `-rllvm-benchmark`
in a private source snapshot. Native API checks and extracted LLVM
string-constant checks require the changed value; the source is restored
afterward. When this baseline was packaged, the [source
audit](source-preservation.json) of the original example checkouts found nghttp2
at `ea02d8bc7d15f4b899ebf32c52e5934c246127da` rather than its pin and quiche at
`f0c7193c3b130d766f0d6f3e75d4f2405c85d376` rather than its pin and with 6
changed or untracked paths. Measurements used the prepared snapshots at the
pinned commits listed below.

## Evidence and offline reproduction

- The externally retained `records.tar.xz` preserves the original run and
  preparation group manifests, each run's `run.json` and indexed JSONL streams
  (commands, operations, validations, diagnostics, and final samples), the
  report generated from them, and 3 supplied context files.
  [evidence-manifest.json](evidence-manifest.json) lists SHA256 hashes and byte
  lengths of the original files.
- Full build trees, bitcode, tool stdout/stderr, and diagnostic receipts remain
  outside Git. Their recorded paths and hashes identify retained local evidence;
  rerun the workflow to recreate these large artifacts elsewhere.

The archive is 34,726,552 bytes; its SHA256 is
`18146bde73b75b5cfefba554796aadbd4b69cc23e0b5052dfa3aa9f0a3eb0590`. It is
excluded from Git and retained separately. Given the archive, regenerate the
full report without building anything:

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
Build provenance must describe the binaries actually used.

```bash
uv run python -m benchmarks prepare \
  --profile nghttp2-c-cmake \
  --profile nghttp2-cxx-cmake \
  --profile quiche-cargo \
  --examples-root "$EXAMPLES_ROOT" \
  --dependencies-root "$DEPENDENCIES_ROOT" \
  --tool-root "$RLLVM_BIN" --tool-root "$LLVM_BIN" \
  --tool-root "$RUST_BIN" --tool-root "$BUILD_TOOLS_BIN" \
  --tool-root "$SYSTEM_BIN" --output "$PREPARED"

uv run python -m benchmarks run \
  --manifest "$PREPARED/prepared-group.json" \
  --profile nghttp2-c-cmake \
  --profile nghttp2-cxx-cmake \
  --profile quiche-cargo \
  --baseline --repetitions 4 --jobs 8 --seed 144 \
  --extraction-repeats 2 \
  --rllvm-provenance "$PROVENANCE" --output "$RUN_ROOT"
```

The full source, submodule, lockfile, flags, tool hashes, dependency versions,
and effective commands are retained per run. Source pins are:

| Project | Commit |
|---|---|
| nghttp2 | `a49ccc728863e7d8e5da369552e889e95399bd37` |
| quiche | `c8da372daa06b7cb51aa23b1a55bfe395dcf3d46` |

This directory was generated by `uv run python -m benchmarks baseline` from the
saved records.
