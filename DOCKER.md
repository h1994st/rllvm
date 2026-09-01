# Docker Usage

Run rllvm in a container with LLVM/Clang pre-installed — no local toolchain setup required.

## Build the Image

```bash
docker build -t rllvm .
```

## Usage

### Interactive Shell

```bash
docker run --rm -it -v "$(pwd)":/workspace rllvm
```

This drops you into a bash shell at `/workspace` (your mounted project directory) with all rllvm tools on the PATH.

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

### Build a Project with make

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
| `rllvm-init` | Auto-detect LLVM installation and generate config |
| `rllvm-info` | Analyze and inspect bitcode files |
| `rllvm-rustc` | Rust compiler wrapper for bitcode extraction |
| `rllvm-completions` | Generate shell completions |

## Configuration

Mount a config file to persist settings:

```bash
docker run --rm -it \
  -v "$(pwd)":/workspace \
  -v "$HOME/.rllvm":/root/.rllvm \
  rllvm
```

Or generate a fresh config inside the container:

```bash
docker run --rm -it -v "$(pwd)":/workspace rllvm -c "rllvm-init"
```
