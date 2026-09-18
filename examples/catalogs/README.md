# Catalogs + rllvm Example

A catalog is a JSON record of which modules were found and where each came
from. Use one to extract a subset rather than the whole program.

## Build and verify

```bash
./check.sh
```

It builds three translation units, writes a catalog, selects one module by its
source path, and checks the extracted bitcode carries that function and not
the others.

## What it does

```bash
rllvm-info build/app --json > build/catalog.json
rllvm-get-bc build/app --source "$PWD/src/parser/parse.c" --output-dir build/selected
rllvm-get-bc build/selected/catalog.json -o build/selected.bc
```

`--module`, `--source` and `--configuration` are repeatable: alternatives
within one option combine, different options intersect, and an unmatched
selector fails. `--output-dir` must be new, and copies hash-checked modules
into it with relative paths so the directory can move.

## Inspect the result

```bash
python3 -m json.tool build/catalog.json | head -40
llvm-nm --defined-only build/selected.bc
```

See the [format reference](../../docs/CATALOG.md).
