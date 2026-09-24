# FFI + rllvm Example

Two small programs whose call graphs cross the FFI boundary, one led from each
language, so the queries have something to answer on both sides:

```text
Rust-led    main::main ─▶ c_double ─▶ rust_add        main.rs, c_side.c
            main::main ─▶ cxx_triple                  cxx_side.cc

C-led       main ─▶ rust_scale ─▶ c_offset            c_main.c, rust_side.rs
```

## Build and verify

```bash
./check.sh
```

## What it does

C and C++ compile through the wrappers into one archive; `rllvm-rustc` builds
the Rust binary against it:

```bash
rllvm-cc  -g -c c_side.c   -o build/c_side.o
rllvm-cxx -g -c cxx_side.cc -o build/cxx_side.o
llvm-ar rcs build/libffidemo.a build/c_side.o build/cxx_side.o
rllvm-rustc -g main.rs -o build/app -L build -l static=ffidemo

rllvm-get-bc build/app -o build/app.bc
rllvm-info build/app --json >build/catalog.json
```

One module holds all three languages, and the queries report each crossing at
the line that makes the call:

```bash
# Ask by the readable name; the mangled symbol works too
rllvm-query --catalog build/catalog.json callees 'main::main'
#   -> c_double    at main.rs:12
#   -> cxx_triple  at main.rs:13

rllvm-query --catalog build/catalog.json callers rust_add
#   -> c_double    at c_side.c:3
```

The C-led program inverts it: `rllvm-rustc` builds a staticlib and `rllvm-cc`
links the `main` that calls into it.

```bash
rllvm-rustc -g --crate-type staticlib rust_side.rs -o build/librustside.a
rllvm-cc -g c_main.c build/librustside.a -o build/app_c

rllvm-query --catalog build/catalog_c.json callers rust_scale # main,       c_main.c:8
rllvm-query --catalog build/catalog_c.json callers c_offset   # rust_scale, rust_side.rs:7
```

`ffi-exports` lists what the Rust side makes callable from C, straight from
the catalog:

```bash
rllvm-query --catalog build/catalog.json ffi-exports    # rust_add
rllvm-query --catalog build/catalog_c.json ffi-exports  # rust_scale
```

Its module holds three functions, not the whole Rust runtime: the staticlib
pulls in a prebuilt std that was never built through the wrapper.

The program prints `doubled=42 tripled=126`: 21 doubled through C, which
reaches the doubling by calling back into Rust, then tripled through C++.
`check.sh` asserts that line, so a boundary that stopped working would fail
before any query runs.

## Why `-g` on every side

Without debug info the calls still resolve — the call graph comes from the IR,
not from DWARF — but every answer carries a null location. The crossing is
still found; you just cannot say where it is written.

## What this does not establish

Three languages reaching one module is not whole-program completeness. Anything
linked without the wrappers contributes no bitcode, and a library implemented in
assembly contributes none either, whatever its language. The answers describe
the modules that were captured.

See [external/quiche](../external/quiche/) for the same queries on a real
codebase, where BoringSSL's assembly routines are exactly that case.
