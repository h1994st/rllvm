# LTO + rllvm Example

Set `lto_mode` in the config or `RLLVM_LTO_MODE`, and use the same mode when
compiling and linking.

| Mode | Captured bitcode | Support |
| --- | --- | --- |
| `marker` (default) | per-source modules recorded in the LTO objects | Full and ThinLTO; ELF and Mach-O; C/C++ |
| `save-temps` | the full-LTO linker's merged, optimized module | separate links and combined compile-and-link |
| `skip` | nothing extra; emits a warning | explicitly disabling capture for LTO |

`save-temps` needs real LTO inputs — adding `-flto` only at link time is not
enough — and ThinLTO has no single merged module, so use `marker` for it. COFF
and WebAssembly reject `marker` and direct you to `skip`.

## Build and verify

```bash
./check.sh
```

It builds the program under `marker` and `save-temps` and checks each
extraction defines `main` and `helper`, then checks a static library of
`-flto` objects extracts `helper`.

## What it does

```bash
RLLVM_LTO_MODE=marker     rllvm-cc -flto lib.c app.c -o build/marker/app
RLLVM_LTO_MODE=save-temps rllvm-cc -flto lib.c app.c -o build/save-temps/app
rllvm-get-bc build/marker/app -o build/marker/app.bc
```

A static library of `-flto` objects extracts without linking a program: its
members are bitcode, and each one is taken as a module.

```bash
rllvm-cc -flto -c lib.c -o build/archive/lib.o
llvm-ar rcs build/archive/libhelper.a build/archive/lib.o
rllvm-get-bc build/archive/libhelper.a -o build/archive/libhelper.bc
```

## Inspect the result

```bash
llvm-nm --defined-only build/marker/app.bc
llvm-nm --defined-only build/save-temps/app.bc
```

`helper` is external under `marker` and local under `save-temps`: full LTO
merged the modules and internalized it.
