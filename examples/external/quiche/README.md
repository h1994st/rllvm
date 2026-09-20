# quiche + rllvm

[quiche](https://github.com/cloudflare/quiche) is Cloudflare's QUIC and HTTP/3
library: a Cargo workspace that also builds BoringSSL through a build script.

No `check.sh` — see [external/](../README.md).

## Build and extract

```bash
git clone --recursive https://github.com/cloudflare/quiche && cd quiche

RUSTC_WRAPPER=rllvm-rustc cargo build --release -p quiche

rllvm-get-bc target/release/libquiche.rlib -o quiche.bc
rllvm-info quiche.bc
```

## What ends up captured

`RUSTC_WRAPPER` captures what Cargo compiles. BoringSSL is not that: a build
script compiles it through a C compiler, so add `CC=rllvm-cc` to capture it
too.

| Build | Extract from | Functions |
|---|---|---|
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.rlib` | 557 |
| `RUSTC_WRAPPER=rllvm-rustc` | `target/release/libquiche.a` | 1697 |
| plus `CC=rllvm-cc` | `target/release/libquiche.a` | 7376 |

The `.rlib` is the `quiche` crate alone; the `.a` adds the dependency crates
linked into it. Without `CC`, the crypto names in there belong to the `boring`
bindings crate — BoringSSL's own `X509`, `BIO` and `bssl::` code only appears
in the third row.

## Queries across the FFI boundary

With `CC=rllvm-cc` the C side is captured too, so a call path can be followed
out of Rust and into BoringSSL:

```bash
rllvm-get-bc target/release/libquiche.a --output-dir cat
rllvm-query --catalog cat/catalog.json callers SSL_do_handshake
```

```text
quiche::tls::Handshake::do_handshake   at quiche/src/tls/mod.rs:556
SSL_accept                             at boringssl/ssl/ssl_lib.cc:770
```

The Rust call site and the C++ definition are both reported from source.
`reach` follows it further, through the boundary and into BoringSSL's
internals:

```bash
rllvm-query --catalog cat/catalog.json reach \
  _RNvMs2_NtCs8f0ESrtUyjS_6quiche3tlsNtB5_9Handshake12do_handshake ssl_send_alert
```

```text
<quiche::tls::Handshake>::do_handshake     Rust
SSL_do_handshake                           C ABI
bssl::ssl_run_handshake(...)               C++
bssl::ssl_handle_open_record(...)          C++
bssl::ssl_send_alert(ssl_st*, int, int)    C++
```

## What the answers do not cover

`EVP_AEAD_CTX_seal` and `SHA256_Update` resolve to nothing: BoringSSL
implements them in assembly, so there is no bitcode to capture and `reach`
reports the absence rather than inventing a path.

The modules also disagree about the target triple — 282 from clang say
`arm64-apple-macosx26.5.0`, 16 from rustc say `arm64-apple-macosx11.0.0`,
being rustc's deployment target. They merge and analyse anyway; all 298 carry
debug info and none failed to parse.

## Validated against

quiche `4d23d859` on arm64 macOS, built with rustc 1.98.0 (LLVM 22.1.8) and
extracted with LLVM 23.1.1. A reader handles its own major and older, so the
newer tools read what rustc emitted.
