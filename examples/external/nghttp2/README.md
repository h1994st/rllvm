# nghttp2, nghttp3, ngtcp2 + rllvm

Three C libraries that build the same way: CMake, no wrapper-specific changes.
One directory rather than three near-identical ones — only the names differ.

No `check.sh` — see [external/](../README.md).

## Build and extract

```bash
git clone https://github.com/nghttp2/nghttp2 && cd nghttp2

export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_LIB_ONLY=ON -DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF
cmake --build build

rllvm-get-bc build/lib/libnghttp2.a -o nghttp2.bc
rllvm-info nghttp2.bc
```

`ENABLE_LIB_ONLY` builds only the library and skips the applications, which are
what pull in OpenSSL, libev and zlib. Drop it to capture those as well, with
their dependencies installed as usual.

The static library is the tidiest target: it carries the same name on every
platform, while the shared one is `libnghttp2.dylib` on macOS and
`libnghttp2.so` on Linux. Either extracts.

## The other two

Same commands, different repository and artifact:

| Project | Repository | Extract from |
|---|---|---|
| nghttp2 | `github.com/nghttp2/nghttp2` | `build/lib/libnghttp2.a` |
| nghttp3 | `github.com/ngtcp2/nghttp3` | `build/lib/libnghttp3.a` |
| ngtcp2 | `github.com/ngtcp2/ngtcp2` | `build/lib/libngtcp2.a` |

## Validated against

Clang 23.1.1 on arm64 macOS, at nghttp2 `140157a8`, nghttp3 `2304973`, ngtcp2
`3c23148e`: 468, 393 and 795 functions in the extracted modules.
