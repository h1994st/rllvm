# Docker Usage

Run rllvm in a container with an LLVM/Clang toolchain already configured — no
local setup required.

## Build the Image

```bash
docker build -t rllvm .
```

The toolchain comes from apt.llvm.org, not the distro, and defaults to the LLVM
major CI pins. Override it for a consumer that caps out lower — PhASAR, for
instance, supports 16 to 22:

```bash
docker build --build-arg LLVM_VERSION=22 -t rllvm:llvm22 .
```

A bitcode reader understands its own major and older, never newer, so match
this to whatever produced the bitcode you intend to read.

## Usage

### Interactive Shell

```bash
docker run --rm -it -v "$(pwd)":/workspace rllvm
```

This drops you into a bash shell at `/workspace` (your mounted project
directory) with all rllvm tools on the PATH.

### Compile a Single File

```bash
docker run --rm -v "$(pwd)":/workspace rllvm -c "rllvm-cc -c hello.c -o hello.o"
```

### Extract Bitcode

```bash
docker run --rm -v "$(pwd)":/workspace rllvm -c "\
  rllvm-cc -c hello.c -o hello.o && \
  rllvm-cc hello.o -o hello && \
  rllvm-get-bc hello"
```

### Build a Project

`make`, `cmake` and `ninja` are installed, so a real project builds as it would
on the host:

```bash
docker run --rm -v "$(pwd)":/workspace rllvm -c "\
  export CC=rllvm-cc CXX=rllvm-cxx && \
  make && \
  rllvm-get-bc my_program"
```

### Analyze Bitcode

```bash
docker run --rm -v "$(pwd)":/workspace rllvm -c "rllvm-info hello.bc"
```

## Available Tools

| Binary | Description |
|---|---|
| `rllvm-cc` | C compiler wrapper (wraps clang) |
| `rllvm-cxx` | C++ compiler wrapper (wraps clang++) |
| `rllvm-get-bc` | Extract whole-program bitcode from compiled binaries |
| `rllvm-compdb` | Generate modules from a `compile_commands.json` |
| `rllvm-init` | Auto-detect LLVM installation and generate config |
| `rllvm-info` | Analyze and inspect bitcode files |
| `rllvm-rustc` | Rust compiler wrapper for bitcode extraction |
| `rllvm-completions` | Generate shell completions |

`rllvm-query` is not in the image. It links LLVM statically and ships as its
own crate and formula; see the [README](README.md#queries).

LLD is installed, so `-fuse-ld=lld`, `wasm-ld` and cross-compilation all work.

## Configuration

The image writes `/root/.rllvm/config.toml` at build time, pointing at its own
LLVM. Nothing needs generating on first run.

Mount your own to override it:

```bash
docker run --rm -it \
  -v "$(pwd)":/workspace \
  -v "$HOME/.rllvm":/root/.rllvm \
  rllvm
```

A mounted config must name paths that exist *inside* the container, so a host
config pointing at `/opt/homebrew` or `/usr/local` will not resolve. Regenerate
it in place instead:

```bash
docker run --rm -it -v "$(pwd)":/workspace rllvm -c "rllvm-init"
```
