# LLD + rllvm Example

Builds through [LLD](https://lld.llvm.org/) instead of the platform's default
linker. One flag covers both platforms: clang resolves `-fuse-ld=lld` to
`ld64.lld` on macOS and `ld.lld` on Linux.

## Requirements

LLD, which ships separately from LLVM on some platforms:

```bash
brew install lld            # macOS
sudo apt install lld        # Debian/Ubuntu
```

## Build and verify

```bash
./check.sh
```

It links two translation units through LLD and checks the extracted bitcode
defines both `main` and `helper`.

## What it does

```bash
rllvm-cc -fuse-ld=lld lib.c app.c -o build/app
rllvm-get-bc build/app -o build/app.bc
```

## Why it is worth checking

The recorded bitcode path travels in a section the linker has to carry into
its output. Linkers disagree about which sections they keep, so a capture that
works under one can silently produce a bitcode-free binary under another — the
link succeeds and the loss only surfaces later, at extraction.

## Inspect the result

```bash
llvm-nm --defined-only build/app.bc
```
