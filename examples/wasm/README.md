# WebAssembly + rllvm Example

Builds a wasm32 module through rllvm and extracts whole-program bitcode from the
linked `.wasm`.

## Requirements

- clang with the `wasm32` target (present in standard LLVM builds; check with
  `clang --print-targets | grep wasm32`)
- `wasm-ld`, which ships with LLD rather than LLVM. On Homebrew that is a
  separate formula, and its version must match your LLVM:

  ```bash
  brew install llvm@22 lld@22
  ```

  A mismatched pair fails at link time with a dyld symbol error, not a message
  about versions.

## Build

```bash
./build.sh
```

## What it does

```bash
rllvm-cc --target=wasm32-unknown-unknown -c -o build/lib.o lib.c
rllvm-cc --target=wasm32-unknown-unknown -c -o build/main.o main.c

rllvm-cc --target=wasm32-unknown-unknown -nostdlib \
    -Wl,--no-entry -Wl,--export-all -o build/app.wasm build/lib.o build/main.o

rllvm-get-bc build/app.wasm -o build/app.bc
```

`-nostdlib` and `-Wl,--no-entry` are needed because these sources are
freestanding and define no `main`.

## How it works on WebAssembly

Each object gets a custom section, `.rllvm_bc`, holding the absolute path of its
bitcode. `wasm-ld` concatenates custom sections from its inputs, so the linked
module lists every translation unit, and `rllvm-get-bc` links those bitcode
files into one module.

The section name matters. `wasm-ld` skips `.llvmbc` and `.llvmcmd` by name —
they belong to `clang -fembed-bitcode`, and the Rust toolchain does not want
that data in linked output — while concatenating every other custom section.
Any bitcode path stored under those names is silently dropped at link time.

## Inspect the result

```bash
llvm-objdump --section-headers build/app.wasm | grep rllvm_bc
llvm-dis -o - build/app.bc | grep '^define'
```
