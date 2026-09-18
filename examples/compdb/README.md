# Compilation database + rllvm Example

`rllvm-compdb` compiles selected entries from an existing
`compile_commands.json`, so you can capture bitcode without rebuilding through
the wrappers.

These modules describe the **current source tree**, not membership in a real
link. Use wrapper capture when participation in the actual build matters.

## Build and verify

```bash
./check.sh
```

It writes a small `compile_commands.json`, lists it, generates modules and a
catalog from it, then extracts bitcode from the catalog and checks it defines
`twice`.

## What it does

```bash
mkdir -p build
cat >build/compile_commands.json <<JSON
[
  {
    "directory": "$PWD/build",
    "file": "$PWD/demo.c",
    "command": "clang -c $PWD/demo.c -o $PWD/build/demo.o"
  }
]
JSON

rllvm-compdb list build/compile_commands.json
rllvm-compdb generate build/compile_commands.json --output-dir build/analysis
rllvm-get-bc build/analysis/catalog.json -o build/demo.bc
```

`list` reports entry and configuration IDs without compiling. `generate` takes
all entries by default; repeat `--source` or `--entry` to narrow.

## Inspect the result

```bash
llvm-nm --defined-only build/demo.bc
```
