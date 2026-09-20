# FFI + rllvm Example

A program whose call graph crosses the FFI boundary in both directions, so the
queries have something to answer on each side:

```text
main.rs        main::main  ──calls──▶  c_double      c_side.c
c_side.c       c_double    ──calls──▶  rust_add      main.rs
cxx_side.cc    cxx_triple  ◀──called── main::main
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
rllvm-query --catalog build/catalog.json callers c_double  # main::main, main.rs:12
rllvm-query --catalog build/catalog.json callers rust_add  # c_double,  c_side.c:3
```

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
