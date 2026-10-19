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

Wrapped dependency crates contribute modules when their archive members reach
the link. An `.rlib` also extracts on its own, and the wrapper runs without
Cargo:

```bash
rllvm-get-bc 'target/debug/deps/libmylib-<hash>.rlib' -o mylib.bc
rllvm-rustc main.rs -o app && rllvm-get-bc app -o whole.bc
```

rustc records `app.bc` beside `app`, so extraction needs another `-o`; it
refuses to write over a module it reads.

## What is not captured

- `cargo check` and procedural-macro crates pass through without capture.
- Prebuilt dependencies, including the standard library, are not rebuilt.
- Neither is the allocator shim rustc adds to a staticlib (`__rust_alloc` and
  its siblings); `rllvm-info` lists such members as carrying no bitcode.

Use `RLLVM_LOG_LEVEL=3` for diagnostics under Cargo.

## Inspect the result

```bash
llvm-dis -o - build/rustdemo.bc | grep '^define' | grep helper
```

The symbol carries a per-build crate disambiguator, so it reads like
`_RNvCs<hash>_8rustdemo6helper`.
