# nghttp2, nghttp3, ngtcp2 + rllvm

Three C libraries that build the same way: CMake, no wrapper-specific changes.
One directory rather than three near-identical ones — only the names and a few
flags differ.

No `check.sh` — see [external/](../README.md).

## Build and extract

```bash
git clone https://github.com/nghttp2/nghttp2 && cd nghttp2
git checkout 140157a8

export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_LIB_ONLY=ON -DBUILD_TESTING=OFF \
  -DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF
cmake --build build

rllvm-get-bc build/lib/libnghttp2.a -o nghttp2.bc
rllvm-info nghttp2.bc
```

`ENABLE_LIB_ONLY` builds only the library and skips the applications, which are
what pull in OpenSSL, libev and zlib. Drop it to capture those as well, with
their dependencies installed as usual. `BUILD_TESTING=OFF` skips the tests,
which need the `tests/munit` submodule.

The static library is the tidiest target: it carries the same name on every
platform, while the shared one is `libnghttp2.dylib` on macOS and
`libnghttp2.so` on Linux. Either extracts.

## The other two

Same commands, with these differences:

| Project | Clone | Commit | Static library | Extract from |
|---|---|---|---|---|
| nghttp2 | `github.com/nghttp2/nghttp2` | `140157a8` | `-DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF` | `build/lib/libnghttp2.a` |
| nghttp3 | `github.com/ngtcp2/nghttp3`, `--recursive` | `2304973` | built by default | `build/lib/libnghttp3.a` |
| ngtcp2 | `github.com/ngtcp2/ngtcp2` | `3c23148e` | built by default | `build/lib/libngtcp2.a` |

nghttp3's library compiles `lib/sfparse` from a submodule: clone with
`--recursive` and run `git submodule update --init` after the checkout.

## Validated against

Clang 23.1.1 on arm64 macOS, at the commits above: 468, 393 and 795 functions
in the extracted modules.
