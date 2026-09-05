# rllvm

[![CI](https://github.com/h1994st/rllvm/actions/workflows/ci.yml/badge.svg)](https://github.com/h1994st/rllvm/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/h1994st/rllvm/graph/badge.svg?token=PWKZ6H64BS)](https://codecov.io/github/h1994st/rllvm)
[![crates.io](https://img.shields.io/crates/v/rllvm.svg)](https://crates.io/crates/rllvm)

Extract whole-program LLVM bitcode from any build.

Point your build system at rllvm's compiler wrappers, build normally, then pull a
single `.bc` for the whole program back out of the finished binary.

## Features

- **Drop-in compiler wrappers.** `export CC=rllvm-cc` and build. No separator, no
  shim script, no build-system plugin.
- **Rust and cargo.** Wrap `cargo build` and extract from the binary,
  dependency crates included.
- **WebAssembly.** Whole-program bitcode from a linked `wasm32` module, not just
  from individual objects.
- **Relocatable bitcode paths.** Objects normally pin themselves to the directory
  that built them. Record paths relative to a root instead, and extraction keeps
  working after the tree moves, comes out of a container, or is replayed from a
  compiler cache.
- **Merge strategies.** Link everything into one module, stage the merge by
  directory for large projects, or produce a bitcode archive.
- **Single static binary.** No runtime to install; `cargo install rllvm`.
- **Bitcode inspection.** `rllvm-info` reports what a module contains.

## Quick start

Install LLVM/Clang, then rllvm:

```bash
brew install llvm                                    # macOS
sudo apt install llvm llvm-dev clang libclang-dev    # Ubuntu / Debian

cargo install rllvm
```

Build something and extract its bitcode:

```bash
rllvm-cc -o hello hello.c
rllvm-get-bc hello              # produces hello.bc
```

Or point an existing project at it:

```bash
export CC=rllvm-cc CXX=rllvm-cxx

./configure && make             # autotools
cmake -B build && cmake --build build

rllvm-get-bc build/my_program
```

On first run rllvm writes a config with tool paths discovered from `llvm-config`.

## Usage

### Extracting

```bash
rllvm-get-bc hello                          # executable  -> hello.bc
rllvm-get-bc libfoo.a                       # archive     -> libfoo.a.bc
rllvm-get-bc -b libfoo.a                    # bitcode archive -> libfoo.bca
rllvm-get-bc --merge-strategy partial prog  # stage the merge by directory
rllvm-get-bc -m hello                       # also write hello.bc.manifest
rllvm-get-bc -o out.bc hello
```

### Wrapper flags

Wrapper options are long-only and prefixed `--rllvm-`, so they cannot collide
with a compiler flag. Everything else — including `-c`, `-v`, `--help` and
`--version` — goes straight to the compiler, because build systems identify the
compiler by running `$CC --version`.

```
--rllvm-compiler <PATH>   Override the wrapped compiler path
--rllvm-verbose[=LEVEL]   Log verbosity; bare flag is level 1, max 4
--rllvm-help              Print help for the wrapper
--rllvm-version           Print the wrapper version
```

A `--` separator is still accepted, so existing shim scripts keep working.

### CMake toolchain file

```bash
cmake -B build -DCMAKE_TOOLCHAIN_FILE=path/to/rllvm/cmake/rllvm-toolchain.cmake
cmake --build build
rllvm-get-bc build/my_program
```

See [`examples/cmake/`](examples/cmake/).

### Rust and cargo

```bash
RUSTC_WRAPPER=rllvm-rustc cargo build
rllvm-get-bc target/debug/my_program
```

Every crate in the graph contributes, so the extracted module covers
dependencies as well as the binary's own code. A library crate works on its
own:

```bash
rllvm-get-bc target/debug/deps/libmylib-<hash>.rlib
```

### WebAssembly

```bash
rllvm-cc --target=wasm32-unknown-unknown -c -o lib.o lib.c
rllvm-cc --target=wasm32-unknown-unknown -nostdlib -Wl,--no-entry \
    -o app.wasm lib.o main.o
rllvm-get-bc app.wasm -o app.bc
```

Linking needs `wasm-ld`, which ships with LLD rather than LLVM and must match
your LLVM version. See [`examples/wasm/`](examples/wasm/).

### Relocatable bitcode paths

By default an object records the **absolute** path of its bitcode, which pins it
to the directory that built it. Set a root to record paths relative to it, then
name the root again when extracting:

```bash
export RLLVM_BITCODE_ROOT=/path/to/build
make

# later, after the tree has moved:
rllvm-get-bc --bitcode-root /new/path/to/build prog -o prog.bc
```

Objects built without a root keep absolute paths and are unaffected — the
extractor tells the two apart by the leading separator, so both forms can appear
in the same binary.

## Configuration

A TOML file, created on first run with paths inferred from `llvm-config`. It
lives at `$RLLVM_CONFIG` if set, otherwise `~/.rllvm/config.toml`.

| Key | Required | Description |
| --- | --- | --- |
| `llvm_config_filepath` | Yes | Absolute path to `llvm-config` |
| `clang_filepath` | Yes | Absolute path to `clang` |
| `clangxx_filepath` | Yes | Absolute path to `clang++` |
| `llvm_ar_filepath` | Yes | Absolute path to `llvm-ar` |
| `llvm_link_filepath` | Yes | Absolute path to `llvm-link` |
| `llvm_objcopy_filepath` | No | Absolute path to `llvm-objcopy`; preferred for embedding, with an internal fallback |
| `rustc_filepath` | No | Absolute path to `rustc` (default: `rustc` on `PATH`) |
| `bitcode_store_path` | No | Directory for intermediate bitcode files (must be absolute) |
| `bitcode_root` | No | Record embedded paths relative to this root (default: absolute) |
| `llvm_link_flags` | No | Extra flags for `llvm-link` |
| `lto_ldflags` | No | Extra flags for link-time optimization |
| `bitcode_generation_flags` | No | Extra flags for bitcode generation (e.g. `-flto`) |
| `lto_mode` | No | How `-flto` builds record bitcode: `marker` (default), `save-temps`, `skip`; `RLLVM_LTO_MODE` overrides |
| `is_configure_only` | No | Skip bitcode generation entirely (default: `false`) |
| `cache_enabled` | No | Reuse bitcode across rebuilds; also `RLLVM_CACHE=1` (default: `false`) |
| `log_level` | No | 0=error (default), 1=warn, 2=info, 3=debug, 4+=trace |

```toml
llvm_config_filepath = '/opt/homebrew/opt/llvm/bin/llvm-config'
clang_filepath = '/opt/homebrew/opt/llvm/bin/clang'
clangxx_filepath = '/opt/homebrew/opt/llvm/bin/clang++'
llvm_ar_filepath = '/opt/homebrew/opt/llvm/bin/llvm-ar'
llvm_link_filepath = '/opt/homebrew/opt/llvm/bin/llvm-link'
log_level = 3
```

## How it works

The wrappers run clang normally and, for each source, also emit a `.bc`. The
absolute path of that `.bc` is written into a custom section of the object file,
newline-terminated. The linker concatenates those sections, so the finished
binary carries a list of every translation unit that went into it.
`rllvm-get-bc` reads that list and links the bitcode into one module.

```
source.c ──► rllvm-cc ──► object file (with embedded .bc path)
                              │
                              ▼
executable ◄── linker ◄── object files
                              │
                              ▼
                        rllvm-get-bc ──► whole-program.bc
```

`rllvm-rustc` does the same per crate. A crate that links carries the path in a
marker object added to the link; a crate that produces an `.rlib` carries it in
the archive's members, so a dependency brings its bitcode wherever it is used.

### LTO

With `-flto` the compiler writes a bitcode module where an object file
belongs, so there is no section to record a path in. `lto_mode` picks what
happens instead.

`marker` (default) compiles a marker module naming the bitcode and merges it
into the LTO object with `llvm-link`. Covers full and thin LTO, ELF and
Mach-O, C and C++, and mixes with objects built without `-flto`. Costs one
extra compile and one `llvm-link` per translation unit.

`save-temps` appends the linker's save-temps flag, then collects the
whole-program module the LTO pipeline merged, recording its path instead of
per-unit paths. No per-unit compile. Full LTO only — ThinLTO builds no such
module, and that case warns and collects nothing rather than failing the
build. Needs a separate link step through `rllvm-cc`: a single-step `rllvm-cc
-flto a.c b.c -o prog` is a compile, not an LTO link, so save-temps cannot
hook it. The inputs must be LTO bitcode: `-flto` in `LDFLAGS` alone produces no
merged module, and rllvm errors. The collected module is post-optimization —
the module the linker generated code from.

`skip` generates nothing and warns — the old default behaviour.

An LTO link pulls in only the archive members it uses, so an unused member's
bitcode path never reaches the binary. COFF and WASM are not supported under
`marker`; `-flto` there is an error directing to `lto_mode = "skip"`. Under
`save-temps`, a link producing no merged module is an error.

## Relationship to gllvm and wllvm

rllvm started as a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) (Go)
and [wllvm](https://github.com/SRI-CSL/whole-program-llvm) (Python), and keeps
the same workflow: set `CC`/`CXX`, build, extract. It has since added
WebAssembly support, a Rust wrapper, relocatable bitcode paths, and merge
strategies.

If gllvm or wllvm already work for you, there is no urgency to switch.

## License

[Apache-2.0](LICENSE)
