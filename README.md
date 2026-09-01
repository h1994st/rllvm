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
rllvm-cc -- -o hello hello.c

# Compile C++ code (wraps clang++)
rllvm-cxx -- -o hello hello.cc
```

Arguments before `--` are rllvm flags; arguments after `--` are passed directly to the underlying compiler.

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
rllvm-cc [OPTIONS] -- <compiler args...>

Options:
  -c, --compiler <PATH>    Override the wrapped compiler path
  -v, --verbose            Increase log verbosity (repeat for more: -vvvvv)
```

## Configuration

rllvm is configured via a TOML file. On first run, a default config is created at `~/.rllvm/config.toml` with tool paths inferred from `llvm-config`.

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
| `llvm_objcopy_filepath`    | Yes      | Absolute path to `llvm-objcopy`                          |
| `bitcode_store_path`       | No       | Directory for intermediate bitcode files (must be absolute) |
| `llvm_link_flags`          | No       | Extra flags passed to `llvm-link`                        |
| `lto_ldflags`              | No       | Extra flags for link-time optimization                   |
| `bitcode_generation_flags` | No       | Extra flags for bitcode generation (e.g., `-flto`)       |
| `is_configure_only`        | No       | Skip bitcode generation entirely (default: `false`)      |
| `log_level`                | No       | 0=off, 1=error, 2=warn, 3=info, 4=debug, 5=trace        |

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

[GPL-3.0](LICENSE)
