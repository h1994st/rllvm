# PhASAR + rllvm Example

[PhASAR](https://github.com/secure-software-engineering/phasar) analyses one
LLVM IR module. rllvm produces it: build through the wrappers, then extract.

No `check.sh` — see [external/](../README.md).

## Setup

PhASAR supports LLVM 16 through 22. Install its dependencies, and point rllvm
at the same toolchain so the bitcode it captures is a version PhASAR can read:

```bash
./utils/InstallAptDependencies.sh --noninteractive --llvm-version=22
rllvm-init --llvm-prefix /usr/lib/llvm-22
```

The [container](../../../DOCKER.md) pins both:

```bash
docker build --build-arg LLVM_VERSION=22 -t rllvm:llvm22 .
```

## Whole program

```bash
export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DPHASAR_LLVM_VERSION=22.1
cmake --build build

rllvm-get-bc build/tools/phasar-cli/phasar-cli -o phasar-cli.bc
phasar-cli -m phasar-cli.bc -D ifds-solvertest \
  --auto-globals=false --emit-raw-results --entry-points=__ALL__
```

`PHASAR_LLVM_VERSION` needs the minor: LLVM's CMake package accepts a request
only when major and minor match, so a bare `22` is refused by 22.1.8.

A Release build uses ThinLTO, and the executable still extracts — rllvm records
each source module in a marker that the LTO link carries into the binary.

## A single library

Under ThinLTO an archive holds raw bitcode rather than objects, so there is no
object left to carry a recorded path; turn it off to extract from a library:

```bash
export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build-nolto -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DPHASAR_LLVM_VERSION=22.1 \
  -DPHASAR_ALLOW_LTO_IN_RELEASE_BUILD=OFF
cmake --build build-nolto

rllvm-get-bc build-nolto/lib/Utils/libphasar_utils.a -o phasar-utils.bc
phasar-cli -m phasar-utils.bc -D ifds-solvertest \
  --auto-globals=false --emit-raw-results --entry-points=__ALL__
```

Not `-b`: that writes a bitcode archive, and `--module` wants a module.

## Validated against

PhASAR v2604 with LLVM 22.1.8. `phasar-cli.bc` is 12.8 MB and 8584 functions;
`phasar-utils.bc` is 334.
