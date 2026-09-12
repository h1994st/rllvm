# rllvm

[![CI](https://github.com/h1994st/rllvm/actions/workflows/ci.yml/badge.svg)](https://github.com/h1994st/rllvm/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/h1994st/rllvm/graph/badge.svg?token=PWKZ6H64BS)](https://codecov.io/github/h1994st/rllvm)
[![crates.io](https://img.shields.io/crates/v/rllvm.svg)](https://crates.io/crates/rllvm)

Extract whole-program LLVM bitcode from any build.

Point your build system at rllvm's compiler wrappers, build normally, then pull a
single `.bc` for the whole program back out of the finished binary.

## Features

- **Drop-in C/C++ wrappers.** Set `CC=rllvm-cc CXX=rllvm-cxx` and build, including
  builds that pass compiler arguments through GNU response files.
- **Rust and Cargo.** Capture bitcode from the application and its wrapped
  dependency crates.
- **WebAssembly and eBPF.** Extract from linked `wasm32` modules as well as
  objects, and from BPF objects before or after `bpftool gen object`.
- **LTO.** Capture per-source bitcode under full or ThinLTO, or collect a full-LTO
  linker's merged module.
- **Relocatable paths.** Keep extraction working when the build tree and its
  bitcode files move together.
- **Merge and inspect.** Link modules, stage a merge by directory, or produce a
  bitcode archive; inspect the result with `rllvm-info`.

## Quick start

Install rllvm:

```bash
brew install h1994st/tap/rllvm    # macOS and Linux, prebuilt
cargo install rllvm              # from source
```

rllvm drives an LLVM/Clang toolchain rather than bundling one, so it needs one
to run:

```bash
brew install llvm                                    # macOS
sudo apt install llvm llvm-dev clang libclang-dev    # Ubuntu / Debian
```

On first run rllvm writes a config with tool paths discovered from `llvm-config`.
Set `RLLVM_CONFIG` to use a different configuration file.

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

## Usage

### Extracting

```bash
rllvm-get-bc hello                             # executable -> hello.bc
rllvm-get-bc libfoo.a                          # archive -> libfoo.a.bc
rllvm-get-bc --merge-strategy archive libfoo.a  # bitcode archive -> libfoo.bca
rllvm-get-bc --merge-strategy partial hello    # merge by directory, then combine
rllvm-get-bc -m hello                          # also write hello.bc.manifest
rllvm-get-bc -o out.bc hello                   # choose the output path
```

Outputs default to the current directory. `-m` writes the contributing bitcode
paths to a manifest beside the input. `-b` remains a shorthand for archive mode.
Archive extraction replaces an existing output with the current modules, so
modules removed from the input do not remain in the archive.

Extraction includes the objects and archive members that carry rllvm metadata.
Wrap every compilation whose code you need to capture; prebuilt native inputs
do not acquire bitcode merely by being linked into a wrapped build.

### Inspecting bitcode

```bash
rllvm-info hello.bc       # target, function, basic-block and instruction counts
rllvm-info -f hello.bc    # also list functions and their counts
```

Inspect the extracted `.bc` for a whole-program view. Given an object or binary,
`rllvm-info` inspects only its first recorded module, when that file is available.

### Wrapper flags

Wrapper options are long-only and prefixed `--rllvm-`, so they cannot collide
with a compiler flag. Everything else — including `-c`, `-v`, `--help` and
`--version` — goes straight to the compiler, because build systems identify the
compiler by running `$CC --version`.

```text
--rllvm-compiler <PATH>   Override clang/clang++ (C/C++ wrappers only)
--rllvm-verbose[=LEVEL]   Log verbosity; bare flag is 1, level 4 enables trace
--rllvm-help             Print help for the wrapper
--rllvm-version          Print the wrapper version
```

Place wrapper options before compiler arguments. Use `=` when supplying a
verbosity level; diagnostics go to stderr:

```bash
rllvm-cc --rllvm-verbose=3 -pthread -c hello.c -ohello.o
```

Both `-o hello.o` and `-ohello.o` are accepted. A `--` separator is still
supported for existing shim scripts.

### Response files

`rllvm-cc` and `rllvm-cxx` accept GNU-style UTF-8 compiler response files:

```bash
printf '%s\n' '-O2 -c hello.c -ohello.o' > compile.rsp
rllvm-cc @compile.rsp
rllvm-get-bc hello.o
```

Quoting and nested `@file` references follow Clang's GNU response syntax. Relative
response paths resolve from the compiler's working directory. Large generated
compiler and LLVM-tool commands also use response files when needed.

### CMake toolchain file

```bash
cmake -B build -DCMAKE_TOOLCHAIN_FILE=path/to/rllvm/cmake/rllvm-toolchain.cmake
cmake --build build
rllvm-get-bc build/my_program
```

See [`examples/cmake/`](examples/cmake/).

### Rust and Cargo

```bash
RUSTC_WRAPPER=rllvm-rustc cargo build
rllvm-get-bc target/debug/my_program
```

Wrapped dependency crates contribute their recorded modules when their archive
members reach the link. A library crate can also be extracted directly:

```bash
rllvm-get-bc 'target/debug/deps/libmylib-<hash>.rlib'
```

Replace `<hash>` with the actual artifact hash. Direct invocation also supports
relative output paths:

```bash
rllvm-rustc main.rs -o app
rllvm-get-bc app -o app.bc
```

For direct invocation, `RLLVM_REAL_RUSTC` overrides `rustc_filepath` in the config,
with `rustc` on `PATH` as the fallback. Under `RUSTC_WRAPPER`, Cargo supplies the
compiler path. Use `RLLVM_LOG_LEVEL=3` for wrapper diagnostics under Cargo;
`--rllvm-verbose=3`, `--rllvm-help`, and `--rllvm-version` are available when
invoking the wrapper yourself.

`cargo check` and procedural-macro crates pass through without bitcode capture.
Prebuilt dependencies, including the supplied standard library, are not rebuilt
by the wrapper. Use LLVM tools compatible with the LLVM version reported by
`rustc -vV` when extracting Rust bitcode.

### WebAssembly

```bash
rllvm-cc --target=wasm32-unknown-unknown -c lib.c -o lib.o
rllvm-cc --target=wasm32-unknown-unknown -c main.c -o main.o
rllvm-cc --target=wasm32-unknown-unknown -nostdlib -Wl,--no-entry \
    -o app.wasm lib.o main.o
rllvm-get-bc app.wasm -o app.bc
```

Linking needs `wasm-ld`, which ships with LLD rather than LLVM and must match
your LLVM version. See [`examples/wasm/`](examples/wasm/).

### eBPF

BPF objects are ELF, so the ordinary wrappers apply:

```bash
rllvm-cc --target=bpf -O2 -g -c prog.c -o prog.o
rllvm-get-bc prog.o -o prog.bc
```

The recorded module is BPF IR, and the object's BPF payload is unchanged.
`libbpf` reports `skipping unrecognized data section .rllvm_bc` and loads the
object as it would an unwrapped one.

Objects combined by `bpftool gen object`, or by `libbpf`'s linker directly,
keep every contributing path, so extraction from a linked object covers the
whole program. That linker requires BTF, so compile with `-g`.

### Bitcode storage and relocation

C/C++ bitcode files are hidden files beside the requested output by default.
Their names distinguish the source, output, compiler, and compilation settings,
so separate build variants keep separate bitcode. Set `bitcode_store_path` to an
absolute directory to collect them centrally.

Extraction requires the recorded `.bc` files. Preserve them along with the
objects or binaries, including when restoring outputs from a compiler cache.

By default an object records the **absolute** path of its bitcode, which pins it
to the directory that built it. Set a root to record paths relative to it, then
name the root again when extracting:

```bash
export RLLVM_BITCODE_ROOT=/path/to/build
make

# later, after the tree has moved:
rllvm-get-bc --bitcode-root /new/path/to/build prog -o prog.bc
```

Choose a root containing the bitcode files, including a central store if used.
Absolute and relative entries can coexist; `--bitcode-root` resolves only the
relative ones. Setting a root changes recorded paths, not where bitcode is stored.

### Bitcode caching

The optional C/C++ cache reuses the extra bitcode compilation across rebuilds:

```bash
RLLVM_CACHE=1 cmake --build build   # build must already use the wrappers
```

Caching is off by default. `RLLVM_CACHE=1` enables it and `RLLVM_CACHE=0` disables
it, overriding `cache_enabled`. The default cache directory is `~/.rllvm/cache`;
set `cache_dir` to use another location.

The native compilation still runs. Each lookup preprocesses the current inputs
and checks their dependencies, command, compiler, working directory, and
environment before reusing bitcode. Cache hits therefore still incur
preprocessing work. If validation cannot produce a usable key, rllvm generates
bitcode without caching that result.

Disable caching for changing compiler side inputs that preprocessing does not
capture, such as optimization profiles. Keep compilation inputs stable while a
build runs.

### LTO

Set `lto_mode` in the config or override it with `RLLVM_LTO_MODE`:

| Mode | Captured bitcode | Support |
| --- | --- | --- |
| `marker` (default) | Per-source modules, recorded in the LTO objects | Full and ThinLTO; ELF and Mach-O; C and C++ |
| `save-temps` | The full-LTO linker's merged, post-optimization module | Separate links and combined source/link invocations |
| `skip` | No additional bitcode capture; emits a warning | Use when capture is intentionally disabled for LTO |

For a combined full-LTO build:

```bash
RLLVM_LTO_MODE=save-temps rllvm-cc -flto hello.c -o hello
rllvm-get-bc hello -o hello.bc
```

Use the same mode during compilation and linking. `marker` also supports a mix
of LTO and ordinary objects; fat LTO objects record the bitcode path in both
halves so either linker path can retain it.

`save-temps` requires actual LTO inputs: adding `-flto` only at the link step
cannot produce a merged module from ordinary objects. ThinLTO has no single
merged module to collect, so this mode warns and skips collection for ThinLTO;
use `marker` for ThinLTO builds.
Compiler queries and invocations that do not link do not collect a module.
Linker temporary files explicitly requested by the user are preserved.

COFF and WebAssembly do not support `marker` mode; an LTO invocation there reports
an error directing to `skip`. A `save-temps` link that produces no merged module
is also an error.

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
| `rustc_filepath` | No | Compiler for direct Rust invocation; `RLLVM_REAL_RUSTC` overrides; defaults to `rustc` on `PATH` |
| `bitcode_store_path` | No | Directory for bitcode files (must be absolute; created if missing) |
| `bitcode_root` | No | Record embedded paths relative to this root (default: absolute) |
| `llvm_link_flags` | No | Extra flags for `llvm-link` |
| `lto_ldflags` | No | Extra flags for link-time optimization |
| `bitcode_generation_flags` | No | Extra flags for bitcode generation (e.g. `-flto`) |
| `lto_mode` | No | How `-flto` builds record bitcode: `marker` (default), `save-temps`, `skip`; `RLLVM_LTO_MODE` overrides |
| `is_configure_only` | No | Skip extra C/C++ bitcode work (default: `false`) |
| `cache_enabled` | No | Reuse C/C++ bitcode across rebuilds; overridden by `RLLVM_CACHE` (default: `false`) |
| `cache_dir` | No | Cache directory (default: `~/.rllvm/cache`) |
| `log_level` | No | 0=error (default), 1=warn, 2=info, 3=debug, 4+=trace; `RLLVM_LOG_LEVEL` overrides |

```toml
llvm_config_filepath = '/opt/homebrew/opt/llvm/bin/llvm-config'
clang_filepath = '/opt/homebrew/opt/llvm/bin/clang'
clangxx_filepath = '/opt/homebrew/opt/llvm/bin/clang++'
llvm_ar_filepath = '/opt/homebrew/opt/llvm/bin/llvm-ar'
llvm_link_filepath = '/opt/homebrew/opt/llvm/bin/llvm-link'
log_level = 3
```

`rllvm-init --dry-run` shows a detected configuration without writing it.
Use `rllvm-init --llvm-prefix /path/to/llvm` to choose a toolchain and `-o` to
choose the configuration file to write.

## How it works

The C/C++ wrappers run Clang normally and also emit a `.bc` for each captured
source. Its absolute or root-relative path is written into a custom object
section, newline-terminated. The linker concatenates those sections, preserving
the recorded paths from the objects it includes. `rllvm-get-bc` reads the list,
deduplicates it, and merges or archives the modules.

```
source.c ──► rllvm-cc ──► object file (with embedded .bc path)
                              │
                              ▼
executable ◄── linker ◄── object files
                              │
                              ▼
                        rllvm-get-bc ──► whole-program.bc
```

Universal (multi-`-arch`) builds are not supported in any mode: clang cannot
emit one IR file for two architectures. Build and extract one architecture at a
time. `rllvm-get-bc` cannot read a combined universal binary.

`rllvm-rustc` does the same per crate. A crate that links carries the path in a
marker object added to the link; a crate that produces an `.rlib` carries it in
the archive's members, so a dependency brings its bitcode wherever it is used.

## Workflow benchmarks

The [baseline](benchmarks/baselines/2026-09-12-apple-m4/README.md) compares
validated builds on an Apple M4 with LLVM 22.1.8, eight jobs, and three
repetitions. Clean **build-only** time relative to native compilation:

| Workload | Cache disabled | Primed C/C++ cache |
|---|---:|---:|
| nghttp2 C (CMake) | 1.84× | 1.61× |
| nghttp2 C++ (CMake) | 1.83× | 1.20× |
| Quiche (Cargo) | 1.59× | 1.50× |

Values are median paired ratios; 1× means native build time. Configuration,
extraction, inspection, priming, validation, and diagnostics are excluded.
Cargo uses one codegen unit for target and host crates in both builds.
Filesystem cache and desktop activity were uncontrolled.

C/C++ capture adds a bitcode compilation and embeds its path in object files.
Rust emits bitcode in the same rustc invocation; bitcode writes, marker
compilations, and archive updates add work. The optional bitcode cache covers C/C++,
including Rust projects' native dependencies. Hits skip bitcode compilation
but still preprocess and hash inputs; misses also pay cache storage costs.
Priming helped C++ most, while Rust compilation remains uncached.

Unchanged C/C++ rebuilds stayed near native time. Extraction and inspection add
work even when nothing recompiles: extraction merges modules with `llvm-link`,
and repeated extraction repeats that work. Complete-workflow ratios therefore
include more than compiler-wrapper overhead.

See the [benchmark guide](benchmarks/README.md) for reproduction and coverage
details. `cargo bench` runs the separate Criterion microbenchmarks.

## Relationship to gllvm and wllvm

rllvm started as a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) (Go)
and [wllvm](https://github.com/SRI-CSL/whole-program-llvm) (Python), and keeps
the same workflow: set `CC`/`CXX`, build, extract. It has since added
WebAssembly support, a Rust wrapper, relocatable bitcode paths, and merge
strategies.

The name follows the same convention: `g` for Go, `w` for whole-program-llvm,
`r` for Rust.

[rules_rllvm](https://github.com/h1994st/rules_rllvm) provides separate,
Bazel-native extraction rules and does not use these binaries.

If gllvm or wllvm already work for you, there is no urgency to switch.

## License

[Apache-2.0](LICENSE)
