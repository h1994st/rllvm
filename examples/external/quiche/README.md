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

## Validated against

quiche `4d23d859` on arm64 macOS, built with rustc 1.98.0 (LLVM 22.1.8) and
extracted with LLVM 23.1.1. A reader handles its own major and older, so the
newer tools read what rustc emitted.
