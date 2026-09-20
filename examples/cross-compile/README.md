# Cross-compilation + rllvm Example

Builds for a machine this host is not, and extracts whole-program bitcode for
that target. The example uses `aarch64-unknown-linux-gnu`, which is cross from
an x86_64 Linux host by architecture and from an Apple silicon host by
operating system and object format. On an aarch64 Linux host it would be
neither, so the script skips there rather than pass a native build off as a
cross one.

## Requirements

LLD, which links ELF on any host:

```bash
brew install lld            # macOS
sudo apt install lld        # Debian/Ubuntu
```

## Build and verify

```bash
./check.sh
```

It compiles and links two translation units for aarch64 Linux and checks the
extracted bitcode carries that triple and defines both `_start` and `twice`.

## What it does

```bash
rllvm-cc --target=aarch64-unknown-linux-gnu -fuse-ld=lld -nostdlib \
  lib.c app.c -o build/app
rllvm-get-bc build/app -o build/app.bc
```

`-nostdlib` keeps it to the two translation units, so no cross sysroot is
needed. The binary is never run.

## Why it is worth checking

A cross build picks the linker as much as the code generator. rllvm compiles
each source and then relinks the objects it produced, and that second command
has to carry the target too — without it the relink runs on the host, which
hands ELF objects to the host's linker. On macOS that surfaced as `ld64.lld:
error: unhandled file type`, and on a host whose linker accepts them it would
be worse: a binary built for the wrong machine.

Checking the triple, not just that extraction succeeded, is what separates the
two outcomes.

## Inspect the result

```bash
rllvm-info build/app.bc
llvm-nm --defined-only build/app.bc
```
