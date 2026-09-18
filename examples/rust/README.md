# Rust + rllvm Example

Rust captures bitcode per crate: linked crates carry paths through a marker
object, library crates in archive members.

## Requirements

A Rust toolchain, and LLVM readers compatible with the version `rustc -vV`
reports. rustc bundles its own LLVM, so a reader older than rustc's cannot
parse the bitcode it produces.

## Build and verify

```bash
./check.sh
```

It builds the crate under the wrapper and checks the extracted bitcode defines
`helper`.

## What it does

```bash
CARGO_TARGET_DIR=build/target RUSTC_WRAPPER=rllvm-rustc \
    cargo build --offline --locked
rllvm-get-bc build/target/debug/rustdemo -o build/rustdemo.bc
```

`cargo check` and procedural-macro crates pass through without capture, and
prebuilt dependencies — including the standard library — are not rebuilt. Use
`RLLVM_LOG_LEVEL=3` for diagnostics under Cargo.

## Inspect the result

```bash
llvm-dis -o - build/rustdemo.bc | grep '^define' | grep helper
```

The symbol carries a per-build crate disambiguator, so it reads like
`_RNvCs<hash>_8rustdemo6helper`.
