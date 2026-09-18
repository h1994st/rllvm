# Relocatable build tree + rllvm Example

Recorded bitcode paths are absolute by default, so moving a build tree strands
them. `RLLVM_BITCODE_ROOT` records them relative to a root instead, and
`--bitcode-root` supplies that root's new location at extraction time.

## Build and verify

```bash
./check.sh
```

It builds into `build/`, moves it to `moved/`, checks extraction now fails,
then checks `--bitcode-root` recovers it. Both halves matter: if extraction
still worked after the move, the paths were never relative and the flag would
be proving nothing.

## What it does

```bash
RLLVM_BITCODE_ROOT="$PWD/build" rllvm-cc -c lib.c -o build/lib.o
# ... build, then move build/ to moved/
rllvm-get-bc --bitcode-root moved moved/app -o moved/app.bc
```

The root is resolved against each bitcode file's real path, so a path through
a symlink works. A root that cannot contain the bitcode is reported rather
than silently ignored; run with `RLLVM_LOG_LEVEL=1` to see it.

The root must contain the bitcode files, including a central
`bitcode_store_path` if you use one.

## Inspect the result

```bash
strings moved/app | grep '\.bc'     # relative names, not absolute paths
llvm-nm --defined-only moved/app.bc
```
