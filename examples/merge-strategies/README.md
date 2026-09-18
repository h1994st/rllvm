# Merge strategies + rllvm Example

`rllvm-get-bc` can combine the captured modules four ways.

| Invocation | Result |
| --- | --- |
| `rllvm-get-bc app` | one `.bc` for the whole program |
| `--merge-strategy partial` | groups by directory, then links the groups |
| `--merge-strategy archive libfoo.a` | a `.bca` holding one bitcode member per object |
| `-m` | also writes `app.bc.manifest`, one module path per line |

`partial` produces the same module as the default; it exists for link sets too
large for a single `llvm-link` invocation.

## Build and verify

```bash
./check.sh
```

It builds three translation units across two directories, extracts all four
ways, and checks each result describes the same three units.

## What it does

```bash
rllvm-get-bc build/app -o build/full.bc
rllvm-get-bc --merge-strategy partial build/app -o build/partial.bc
rllvm-get-bc --merge-strategy archive build/libunits.a -o build/libunits.bca
rllvm-get-bc -m build/app -o build/manifest.bc
```

`-o` is worth passing: the default output path is relative to the working
directory, not to the input.

## Inspect the result

```bash
llvm-nm --defined-only build/full.bc
llvm-nm --defined-only build/libunits.bca    # per member
cat build/manifest.bc.manifest
```
