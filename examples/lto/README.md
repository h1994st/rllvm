# LTO + rllvm Example

`-flto` builds record bitcode in one of two ways. Use the same mode when
compiling and linking.

| Mode | Captured bitcode |
| --- | --- |
| `marker` (default) | per-source modules recorded in the LTO objects |
| `save-temps` | the full-LTO linker's merged, optimized module |

`save-temps` needs real LTO inputs — adding `-flto` only at link time is not
enough — and ThinLTO has no single merged module, so use `marker` for it.

## Build and verify

```bash
./check.sh
```

It builds the program under both modes and checks each extraction defines
`main` and `helper`.

## What it does

```bash
RLLVM_LTO_MODE=marker     rllvm-cc -flto lib.c app.c -o build/marker/app
RLLVM_LTO_MODE=save-temps rllvm-cc -flto lib.c app.c -o build/save-temps/app
rllvm-get-bc build/marker/app -o build/marker/app.bc
```

## Inspect the result

```bash
llvm-nm --defined-only build/marker/app.bc
llvm-nm --defined-only build/save-temps/app.bc
```

`helper` is external under `marker` and local under `save-temps`: full LTO
merged the modules and internalized it.
