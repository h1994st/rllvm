# rllvm

[![CI](https://github.com/h1994st/rllvm/actions/workflows/ci.yml/badge.svg)](https://github.com/h1994st/rllvm/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/h1994st/rllvm/graph/badge.svg?token=PWKZ6H64BS)](https://codecov.io/github/h1994st/rllvm)
[![crates.io](https://img.shields.io/crates/v/rllvm.svg)](https://crates.io/crates/rllvm)

Compiler wrappers for building whole-program LLVM bitcode files — a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm)/[wllvm](https://github.com/SRI-CSL/whole-program-llvm).

## How It Works

rllvm provides drop-in compiler wrappers (`rllvm-cc`, `rllvm-cxx`) that transparently run clang/clang++ and simultaneously generate LLVM bitcode. The bitcode file paths are embedded into a special section of each object file. A separate tool (`rllvm-get-bc`) then reads those paths and links all the bitcode into a single whole-program `.bc` file.

```
source.c ──► rllvm-cc ──► object file (with embedded .bc path)
                              │
                              ▼
executable ◄── linker ◄── object files
                              │
                              ▼
                        rllvm-get-bc ──► whole-program.bc
```

## Installation

### Prerequisites

LLVM/Clang must be installed:

```bash
# macOS
brew install llvm

# Ubuntu / Debian
sudo apt install llvm llvm-dev clang libclang-dev
```

### From crates.io

```bash
cargo install rllvm
```

### From source

```bash
git clone https://github.com/h1994st/rllvm.git
cd rllvm
cargo install --path .
```

## Usage

### Compile a single file

```bash
# Compile C code (wraps clang)
rllvm-cc -o hello hello.c

# Compile C++ code (wraps clang++)
rllvm-cxx -o hello hello.cc
```

No `--` separator is needed. Wrapper flags are prefixed `--rllvm-`; everything
else goes to the compiler.

### Extract bitcode

```bash
# Extract linked bitcode from an executable
rllvm-get-bc hello           # produces hello.bc

# Extract from a static library
rllvm-get-bc libfoo.a        # produces libfoo.a.bc

# Build a bitcode archive instead of linking
rllvm-get-bc -b libfoo.a     # produces libfoo.bca

# Save a manifest of individual bitcode file paths
rllvm-get-bc -m hello        # produces hello.bc.manifest

# Specify output path
rllvm-get-bc -o out.bc hello
```

### Build a real project

Use `CC` and `CXX` environment variables to inject rllvm into any build system:

```bash
export CC=rllvm-cc
export CXX=rllvm-cxx

# Autotools
./configure && make

# CMake
cmake -B build && cmake --build build

# Extract bitcode from the final binary
rllvm-get-bc build/my_program
```

### CMake toolchain file

rllvm ships a CMake toolchain file for a more integrated approach:

```bash
cmake -B build -DCMAKE_TOOLCHAIN_FILE=path/to/rllvm/cmake/rllvm-toolchain.cmake
cmake --build build

# Extract bitcode
rllvm-get-bc build/my_program
```

See [`examples/cmake/`](examples/cmake/) for a complete example.

### Wrapper flags

```
rllvm-cc [OPTIONS] <compiler args...>

Usable directly as CC -- no `--` separator required:

    export CC=rllvm-cc && ./configure && make

Wrapper options are long-only and prefixed `--rllvm-`, so they cannot collide
with a compiler flag. Everything else, including `-c`, `-v`, `--help` and
`--version`, is passed straight to the compiler -- build systems identify the
compiler by running `$CC --version`, so the wrapper must not answer it.

Options:
  --rllvm-compiler <PATH>   Override the wrapped compiler path
  --rllvm-verbose[=LEVEL]   Log verbosity; bare flag is level 1, max 4
  --rllvm-help              Print help for the wrapper
  --rllvm-version           Print the wrapper version

`--` is still accepted, so existing shim scripts keep working:

    rllvm-cc --rllvm-verbose=3 -- -o hello hello.c
```

## Configuration

rllvm is configured via a TOML file. On first run, a default config is created with tool paths inferred from `llvm-config`, at `$RLLVM_CONFIG` if set, otherwise `~/.rllvm/config.toml`.

### Config file location

Set the `RLLVM_CONFIG` environment variable to use a custom path:

```bash
export RLLVM_CONFIG=/path/to/config.toml
```

### Config options

| Key                        | Required | Description                                              |
| -------------------------- | -------- | -------------------------------------------------------- |
| `llvm_config_filepath`     | Yes      | Absolute path to `llvm-config`                           |
| `clang_filepath`           | Yes      | Absolute path to `clang`                                 |
| `clangxx_filepath`         | Yes      | Absolute path to `clang++`                               |
| `llvm_ar_filepath`         | Yes      | Absolute path to `llvm-ar`                               |
| `llvm_link_filepath`       | Yes      | Absolute path to `llvm-link`                             |
| `llvm_objcopy_filepath`    | No       | Absolute path to `llvm-objcopy`; used to embed bitcode paths when present, falling back to an internal rewriter |
| `bitcode_store_path`       | No       | Directory for intermediate bitcode files (must be absolute) |
| `bitcode_root`             | No       | Record embedded bitcode paths relative to this root, so objects survive being moved (default: absolute paths) |
| `llvm_link_flags`          | No       | Extra flags passed to `llvm-link`                        |
| `lto_ldflags`              | No       | Extra flags for link-time optimization                   |
| `bitcode_generation_flags` | No       | Extra flags for bitcode generation (e.g., `-flto`)       |
| `is_configure_only`        | No       | Skip bitcode generation entirely (default: `false`)      |
| `log_level`                | No       | 0=error (default), 1=warn, 2=info, 3=debug, 4+=trace     |

### Relocatable bitcode paths

By default an object records the **absolute** path of its bitcode, which pins it
to the directory that built it. That breaks if the tree is moved, copied out of a
container, replayed from a compiler cache into a different tree, or handed to
another CI job.

Set a root to record paths relative to it, then name the root again when
extracting:

```bash
export RLLVM_BITCODE_ROOT=/path/to/build
make

# later, after the tree has moved:
rllvm-get-bc --bitcode-root /new/path/to/build prog -o prog.bc
```

`RLLVM_BITCODE_ROOT` overrides the `bitcode_root` config key. Objects built
without a root keep absolute paths and are unaffected — the extractor tells the
two apart by the leading separator, so both forms can appear in the same binary.

### Example config

```toml
llvm_config_filepath = '/opt/homebrew/opt/llvm/bin/llvm-config'
clang_filepath = '/opt/homebrew/opt/llvm/bin/clang'
clangxx_filepath = '/opt/homebrew/opt/llvm/bin/clang++'
llvm_ar_filepath = '/opt/homebrew/opt/llvm/bin/llvm-ar'
llvm_link_filepath = '/opt/homebrew/opt/llvm/bin/llvm-link'
llvm_objcopy_filepath = '/opt/homebrew/opt/llvm/bin/llvm-objcopy'
bitcode_store_path = '/tmp/bitcode_store'
log_level = 3
```

## Why rllvm?

rllvm is a Rust rewrite of [gllvm](https://github.com/SRI-CSL/gllvm) (Go) and [wllvm](https://github.com/SRI-CSL/whole-program-llvm) (Python). All three tools solve the same problem — extracting whole-program LLVM bitcode — but rllvm offers:

- **Single static binary** — no Go or Python runtime needed; `cargo install` and go.
- **Cross-platform** — tested on Linux and macOS in CI.
- **Drop-in compatible** — same workflow as gllvm/wllvm: set `CC`/`CXX`, build, extract.
- **TOML configuration** — auto-generated config file with LLVM tool paths discovered from `llvm-config`.

If you're already using gllvm or wllvm and they work for you, there's no urgency to switch. rllvm is a good fit if you prefer a self-contained Rust binary or want to integrate with a Rust-based toolchain.

## License

[Apache-2.0](LICENSE)
