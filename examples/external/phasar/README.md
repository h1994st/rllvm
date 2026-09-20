# PhASAR + rllvm Example

[PhASAR](https://github.com/secure-software-engineering/phasar) analyses one
LLVM IR module. Its
[whole-program-analysis guide](https://github.com/secure-software-engineering/phasar/wiki/Whole-Program-Analysis-(using-WLLVM))
reaches for wllvm or gllvm to produce that module; rllvm does the same job, and
the flow is unchanged.

This example has no `check.sh`: it builds a large third-party project from
source before there is anything to capture, which the example harness does not
run.

## Requirements

PhASAR supports LLVM 16 through 22 — `utils/InstallAptDependencies.sh` rejects
anything else. Install its dependencies for the major you intend to use:

```bash
./utils/InstallAptDependencies.sh --noninteractive --llvm-version=22
```

rllvm must drive the same LLVM. A reader understands its own major and older,
never newer, so an rllvm built against a newer LLVM than PhASAR links will
produce bitcode PhASAR cannot read:

```bash
rllvm-init --llvm-prefix /usr/lib/llvm-22
```

The [container](../../../DOCKER.md) pins both at once:

```bash
docker build --build-arg LLVM_VERSION=22 -t rllvm:llvm22 .
```

## Build PhASAR through the wrappers

```bash
export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DPHASAR_LLVM_VERSION=22.1
cmake --build build
```

`PHASAR_LLVM_VERSION` needs the minor. PhASAR passes it to
`find_package(LLVM ...)`, and LLVM's version file accepts a request only when
major *and* minor match, so a bare `22` is refused by 22.1.8. Releases before
LLVM 18 were `x.0`, where a bare major happened to work.

## Extract and analyse

Extract from the **executable**, not from a library:

```bash
rllvm-get-bc build/tools/phasar-cli/phasar-cli -o phasar-cli.bc
phasar-cli -m phasar-cli.bc -D ifds-solvertest \
  --emit-raw-results --entry-points=__ALL__
```

Do not pass `-b`. That selects a bitcode *archive*, and `--module` wants a
module; PhASAR rejects an archive with `error: LLVM module '...' does not
exist!`. The default merges every captured module into one `.bc`, which is what
the analysis reads. gllvm's `-b` means the same thing.

## Why the executable

Two properties of a PhASAR build make a library the wrong target.

`lib/libphasar.a` is an aggregator holding a single object, so extracting from
it yields 14 functions. The code lives across 32 archives, and the real
libraries work — `lib/Utils/libphasar_utils.a` gives 334 — but no one of them
is the program.

More importantly, a Release build turns on ThinLTO
(`PHASAR_ALLOW_LTO_IN_RELEASE_BUILD` defaults to `ON`), so `-flto=thin` makes
clang emit bitcode where object files would be. Archive members are then raw
bitcode with no container to carry a recorded path, and extraction reports
`Unknown file magic`. This is a property of the build, not of the capture tool:
a plain clang build produces the same bitcode members.

Extraction from the linked executable works either way, because the marker
rllvm stages for LTO links survives into the binary. To extract from archives
instead, turn LTO off with `-DPHASAR_ALLOW_LTO_IN_RELEASE_BUILD=OFF`.

## Measured

PhASAR v2604 with LLVM 22.1.8, Release, 10 cores:

| | build | extracted |
|---|---|---|
| clang | 1m49s | — |
| rllvm wrappers | 4m42s | 225 modules |

`phasar-cli.bc` is 12.8 MB and 8584 functions, and PhASAR's IFDS solver runs on
it to completion.
