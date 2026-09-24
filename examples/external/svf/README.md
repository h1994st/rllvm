# SVF + rllvm Example

[SVF](https://github.com/SVF-tools/SVF) analyses one LLVM IR module. Here it
analyses itself: build SVF through the wrappers, extract its `wpa` tool as a
module, and run `wpa` on it.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) runs the flow below in the container and prints
the numbers under [Validated against](#validated-against).

## Setup

SVF supports LLVM 22 from commit `18fb565`; its `build.sh` still downloads
LLVM 21. Build with CMake directly instead, against an LLVM with RTTI such as
the apt.llvm.org packages:

```bash
git clone https://github.com/SVF-tools/SVF
cd SVF
git checkout f3f09503

apt-get update   # prefix both with sudo outside a container
apt-get install -y libz3-dev zlib1g-dev libzstd-dev libncurses-dev
rllvm-init --llvm-prefix /usr/lib/llvm-22
```

The [container](../../../DOCKER.md) pins the toolchain; SVF's dependencies
still need the `apt-get` lines above inside it:

```bash
docker build --build-arg LLVM_VERSION=22 -t rllvm:llvm22 .
```

## SVF analyses itself

```bash
export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DLLVM_DIR=/usr/lib/llvm-22 -DBUILD_SHARED_LIBS=OFF -DSVF_WARN_AS_ERROR=OFF
cmake --build build

rllvm-get-bc build/bin/wpa -o wpa.bc
build/bin/wpa -ander -extapi=build/lib/extapi.bc wpa.bc
```

Static libraries put all of SVF into the `wpa` executable, so one extraction
yields the whole tool. LLVM and Z3 are linked prebuilt and contribute no
bitcode.

`-DSVF_WARN_AS_ERROR=OFF`: Clang 22 warns on an unused variable in
`svf/include/Graphs/CDG.h`, and SVF builds with `-Werror`; plain `clang++`
fails the same way.

`-extapi`: SVF's generated config leaves `SVF_BUILD_DIR` empty, so `wpa` cannot
find the `extapi.bc` it just built unless told where it is.

## Validated against

SVF `f3f09503` with LLVM 22.1.8 in the container on aarch64 Linux. `wpa.bc` is
4.1 MB and 3351 functions; Andersen's analysis takes two to three minutes and
5.0 GB, reporting 377,357 pointers and 13,787 objects.
