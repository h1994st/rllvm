# Name resolution + rllvm Example

A symbol can be named three ways, and the answer always says which one
matched.

| Spelling | `matched` |
| --- | --- |
| `_Z5twiceIiET_S0_` | `mangled` |
| `int twice<int>(int)` | `demangled` |
| `twice` | `fuzzy` |

The mangled symbol is the identity. Demangled readings live in the envelope's
`symbols` table rather than beside each symbol, and a name resolves exactly
before it resolves fuzzily.

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`.

## Build and verify

```bash
./check.sh
```

It checks each spelling finds the one definition and is reported as its own
kind — not just that something was found.

## What it does

```bash
rllvm-query --catalog build/catalog.json defs _Z5twiceIiET_S0_
rllvm-query --catalog build/catalog.json defs 'int twice<int>(int)'
rllvm-query --catalog build/catalog.json defs twice
```

## Inspect the result

```bash
rllvm-query --catalog build/catalog.json defs twice | python3 -m json.tool
```

`resolution` names what was asked for and what it matched; `symbols` carries
the demangled reading.
