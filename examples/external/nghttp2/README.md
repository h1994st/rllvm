# nghttp2, nghttp3, ngtcp2 + rllvm

Three C libraries that build the same way: CMake, no wrapper-specific changes.
One directory rather than three near-identical ones — only the names and a few
flags differ.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) builds all three, runs the queries below and
prints the numbers under [Validated against](#validated-against).

## Build and extract

```bash
git clone https://github.com/nghttp2/nghttp2 && cd nghttp2
git checkout 140157a8

export CC=rllvm-cc CXX=rllvm-cxx
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DENABLE_LIB_ONLY=ON -DBUILD_TESTING=OFF \
  -DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF
cmake --build build

rllvm-get-bc build/lib/libnghttp2.a -o nghttp2.bc
rllvm-info nghttp2.bc
```

`ENABLE_LIB_ONLY` builds only the library and skips the applications, which are
what pull in OpenSSL, libev and zlib. Drop it to capture those as well, with
their dependencies installed as usual. `BUILD_TESTING=OFF` skips the tests,
which need the `tests/munit` submodule. `RelWithDebInfo` keeps debug info, so
query answers carry source lines.

The static library is the tidiest target: it carries the same name on every
platform, while the shared one is `libnghttp2.dylib` on macOS and
`libnghttp2.so` on Linux. Either extracts.

## Querying it

Can bytes from the network reach the HPACK header decoder, and through what?

```bash
rllvm-info build/lib/libnghttp2.a --json > catalog.json

rllvm-query --catalog catalog.json reach nghttp2_session_mem_recv2 nghttp2_hd_inflate_hd_nv
rllvm-query --catalog catalog.json callers nghttp2_hd_inflate_hd_nv
rllvm-query --catalog catalog.json at lib/nghttp2_session.c 3237
```

```text
call              nghttp2_session_mem_recv2
call              session_mem_recv
binding           nghttp2_hd_inflate_hd_nv  (unique, 1 candidate(s))
```

Two calls separate received bytes from the decoder. `callers` adds the public
`nghttp2_hd_inflate_hd*` entry points, each with the line of its call.

Line 3237 is where nghttp2 calls the application's `on_frame_recv_callback`.
The static helper holding that call is inlined into every frame handler, so
`at` lists eleven handlers, and every site is `unresolved`: the application
that registers the callback is not in this module. `reach` cannot follow a path
through a callback, so an empty answer across one is not proof that no path
exists.

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

Clang 23.1.1 on arm64 macOS, at the commits above: 469, 393 and 799 functions
in the extracted modules. `reach` finds the two-call path, and line 3237 has 18
unresolved callback sites across 11 handlers.
