# Source staleness + rllvm Example

A location in an answer points at a source file, and that file may have
changed since the bitcode was built. Every location reports whether it still
matches.

| `source_status` | Meaning |
| --- | --- |
| `current` | the source still matches the recorded digest |
| `modified` | it has changed since the module was built |
| `missing` | the file is no longer there |
| `unknown` | no digest was recorded to check against |

## Requirements

`rllvm-query`, a separate crate that is not built by a default `cargo build`.

## Build and verify

```bash
./check.sh
```

It asks once before editing the source and once after, and checks the answer
changes from `current` to `modified`.

## What it does

```bash
rllvm-query --catalog build/catalog.json defs helper   # location.source_status
# edit the source
rllvm-query --catalog build/catalog.json defs helper   # now modified
```

## Why the digest's origin matters

`status_basis` says *when* the digest was taken, which decides what a match
proves. A `compiler` or `capture` digest dates from the build, so `current`
means the source still matches the bitcode. An `inventory` digest was taken
when the catalog was written, which is after the build: `current` there proves
only that nothing has changed since.
