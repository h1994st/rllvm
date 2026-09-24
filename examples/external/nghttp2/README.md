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

## The other two

Same commands, with these differences:

| Project | Clone | Commit | Static library | Extract from |
|---|---|---|---|---|
| nghttp2 | `github.com/nghttp2/nghttp2` | `140157a8` | `-DBUILD_STATIC_LIBS=ON -DBUILD_SHARED_LIBS=OFF` | `build/lib/libnghttp2.a` |
| nghttp3 | `github.com/ngtcp2/nghttp3`, `--recursive` | `2304973` | built by default | `build/lib/libnghttp3.a` |
| ngtcp2 | `github.com/ngtcp2/ngtcp2` | `3c23148e` | built by default | `build/lib/libngtcp2.a` |

nghttp3's library compiles `lib/sfparse` from a submodule: clone with
`--recursive` and run `git submodule update --init` after the checkout.

## Querying them

Can bytes from the network reach each library's header or packet decoder, and
what does the answer leave out? Two queries per library, shown for nghttp2:

```bash
rllvm-info build/lib/libnghttp2.a --json > catalog.json

rllvm-query --catalog catalog.json reach nghttp2_session_mem_recv2 nghttp2_hd_inflate_hd_nv
rllvm-query --catalog catalog.json at lib/nghttp2_session.c 3237
```

The same two, with the names below, answer for the other two libraries:

| Library | Data enters | Decoder | `reach` finds | Callback called at | Unresolved |
|---|---|---|---|---|---|
| nghttp2 | `nghttp2_session_mem_recv2` | HPACK `nghttp2_hd_inflate_hd_nv` | via `session_mem_recv` | `on_frame_recv_callback`, `lib/nghttp2_session.c:3237` | 18 sites in 10 functions |
| nghttp3 | `nghttp3_conn_read_stream2` | QPACK `nghttp3_qpack_decoder_read_request` | via `nghttp3_conn_read_bidi`, `nghttp3_conn_on_headers` | `recv_data`, `lib/nghttp3_conn.c:1828` | 2 sites in 2 functions |
| ngtcp2 | `ngtcp2_conn_read_pkt_versioned` | QUIC `ngtcp2_pkt_decode_hd_long` | via `conn_recv_pkt` | `recv_stream_data`, `lib/ngtcp2_conn.c:142` | 2 sites in 2 functions |

Every decoder is a few calls from the network. Every callback site is
`unresolved`: the application that registers the callback is not in these
modules, and each helper that makes the call is inlined into several callers.
`reach` cannot follow a path through a callback, so an empty answer across one
is not proof that no path exists — the modules hold 123, 60 and 493 indirect
call sites, none with an LLVM target bound.

## Validated against

Clang 23.1.1 on arm64 macOS, at the commits above: 469, 393 and 799 functions
in the extracted modules, and the paths and unresolved sites in the table.
