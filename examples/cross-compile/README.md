# Cross-compilation + rllvm Example

Builds for machines this host is not, and extracts whole-program bitcode for
each.

Targets `aarch64-unknown-linux-gnu`, plus `riscv64-unknown-linux-gnu` when
`llvm_objcopy_filepath` is set — the embedding fallback does not model RISC-V
relocations. aarch64 Linux is cross from an x86_64 Linux host by architecture,
and from an Apple silicon host by operating system and object format; on an
aarch64 Linux host it is neither, so the script skips there.

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

Each target is built from two translation units, then checked: the extracted
bitcode must carry that triple and define both `_start` and `twice`.

## What it does

```bash
rllvm-cc --target=aarch64-unknown-linux-gnu -fuse-ld=lld -nostdlib \
  lib.c app.c -o build/app
rllvm-get-bc build/app -o build/app.bc
```

`-nostdlib` keeps it to the two translation units, so no cross sysroot is
needed. The binaries are never run.

## Why it is worth checking

A cross build picks the linker as much as the code generator. rllvm compiles
each source and then relinks the objects it produced, and that second command
has to carry the target too — without it the relink runs on the host and hands
ELF objects to the host's linker. Checking the triple, not just that extraction
succeeded, is what catches it.

## Inspect the result

```bash
rllvm-info build/app-aarch64-unknown-linux-gnu.bc
llvm-nm --defined-only build/app-aarch64-unknown-linux-gnu.bc
```
