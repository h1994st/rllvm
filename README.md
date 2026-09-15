# rllvm

[![CI](https://github.com/h1994st/rllvm/actions/workflows/ci.yml/badge.svg)](https://github.com/h1994st/rllvm/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/h1994st/rllvm/graph/badge.svg?token=PWKZ6H64BS)](https://codecov.io/github/h1994st/rllvm)
[![crates.io](https://img.shields.io/crates/v/rllvm.svg)](https://crates.io/crates/rllvm)

Extract whole-program LLVM bitcode from any build.

Point your build system at rllvm's compiler wrappers, build normally, then pull a
single `.bc` for the whole program back out of the finished binary.

## Quick start

Install rllvm and LLVM/Clang:

```bash
brew install h1994st/tap/rllvm llvm   # macOS or Linux with Homebrew
# Or build rllvm from source:
cargo install rllvm
# LLVM dependencies on Ubuntu / Debian:
sudo apt install llvm llvm-dev clang libclang-dev
```

Build, extract, and inspect:

```bash
rllvm-cc hello.c -o hello
rllvm-get-bc hello             # writes hello.bc in the current directory
rllvm-info hello.bc            # target, function, block, and instruction counts
rllvm-info -f hello.bc         # also list individual functions
```

Use `rllvm-cxx` for C++. `rllvm-get-bc` also accepts objects and archives;
`-o out.bc` chooses an output path. Inspect the extracted `.bc` for a
whole-program view: human-readable inspection of a native binary uses only
its first recorded module.

On first run, tool paths are inferred from `llvm-config` and saved in
`~/.rllvm/config.toml`. Set `RLLVM_CONFIG` to use another file, or run
`rllvm-init --dry-run` to preview the detected configuration.

Capture covers the objects and archive members that carry rllvm metadata.
Build every dependency whose bitcode you need through the wrappers; linking
prebuilt native libraries does not capture their code.

## Language support

### C and C++

Point the build system at the wrappers. For Autotools:

```bash
CC=rllvm-cc CXX=rllvm-cxx ./configure
make
rllvm-get-bc path/to/program
```

For CMake, configure a new build directory:

```bash
CC=rllvm-cc CXX=rllvm-cxx cmake -S . -B build
cmake --build build
rllvm-get-bc build/my_program
```

Alternatively, pass
`-DCMAKE_TOOLCHAIN_FILE=path/to/rllvm/cmake/rllvm-toolchain.cmake`.
See the [CMake example](examples/cmake/).

### Rust and Cargo

```bash
RUSTC_WRAPPER=rllvm-rustc cargo build
rllvm-get-bc target/debug/my_program
```

Wrapped dependency crates contribute modules when their archive members reach
the link. Extract an `.rlib` directly, or invoke the wrapper without Cargo:

```bash
rllvm-get-bc 'target/debug/deps/libmylib-<hash>.rlib'
rllvm-rustc main.rs -o app
rllvm-get-bc app
```

Replace `<hash>` with the artifact's hash. Cargo supplies the real compiler;
direct invocation uses `RLLVM_REAL_RUSTC`, then `rustc_filepath`, then `rustc`
on `PATH`. Use `RLLVM_LOG_LEVEL=3` for diagnostics under Cargo.

`cargo check` and procedural-macro crates pass through without capture.
Prebuilt dependencies, including the supplied standard library, are not rebuilt.
Use LLVM readers compatible with the LLVM version reported by `rustc -vV`.

## Advanced usage

### Importing a compilation database

Analyze selected C/C++ sources from an existing `compile_commands.json` without
rebuilding through wrappers:

```bash
rllvm-compdb list build/ > compilations.json
rllvm-compdb generate build/ --source src/example.c --output-dir analysis/example
rllvm-compdb generate build/ --entry ENTRY_ID \
  --extra-arg=-O0 --output-dir analysis/debug --jobs 4
```

`list` reports entry/configuration IDs and unsupported commands without compiling
or requiring sources to exist. `generate` selects all entries by default;
repeat `--source` or `--entry` for alternatives, and combine them to intersect
filters. Source selectors resolve from the current directory. Duplicate
compilations remain distinct; unmatched and empty selections fail.

The output directory must be new. It contains separate modules, diagnostics,
and `catalog.json`. Successful entries survive partial failures, which return
nonzero. Original object and dependency outputs are preserved.

On macOS, macOS-target compilations infer the active SDK with `xcrun` when no
SDK is specified. Set `SDKROOT` or supply `-isysroot`/`--sysroot` in the database
or through `--extra-arg` to choose another SDK. Explicit settings take precedence;
other targets do not receive an inferred macOS SDK.

Only direct `clang`/`clang++` drivers and version-suffixed variants are supported.
Entries use their recorded working directory; relative directories resolve from
the database's directory. Structured `arguments` take precedence over `command`,
which is decoded without a shell. Response files are expanded, and generated
sources/headers must already exist. `--jobs` defaults to one.

The catalog records effective arguments and the supplied or inferred analysis
environment. Implicit Clang configs remain disabled; wrapper configuration flags
do not apply. Launchers, shell operations, multiple-source commands, and flags
with uncontrolled side outputs are unsupported. See the
[catalog reference](docs/CATALOG.md#compilation-database-provenance) for details.

These modules describe the **current source tree**, not executable membership or
a historical build. They are not automatically merged; use wrapper capture when
participation in the real link matters.

### Module catalogs and selection

```bash
rllvm-info app --json > catalog.json
rllvm-info app --json --source src/example.c
rllvm-get-bc app --module MODULE_ID --output-dir analysis/selected
rllvm-get-bc analysis/selected/catalog.json -o selected.bc
```

JSON inventory accepts bitcode, objects, executables, regular archives, and
catalogs. It inspects all selected modules without merging; missing or unreadable
entries remain in the output and cause a nonzero exit. Thin archives are unsupported.

`--module`, `--source`, and `--configuration` are repeatable. Alternatives within
each option are combined; different options intersect. Unmatched selectors fail.
Configuration selection requires recorded metadata, which legacy path sections
do not contain. Replace `MODULE_ID` with an inventory ID.

`--output-dir` copies separate, hash-checked modules into a new directory.
The resulting catalog uses relative module paths and moves with that directory.
Catalogs describe known evidence and selection scope, not proven whole-program
completeness. See the [format reference](docs/CATALOG.md).

### Extraction modes

```bash
rllvm-get-bc --merge-strategy archive libfoo.a  # writes libfoo.bca
rllvm-get-bc --merge-strategy partial app      # merge by directory, then combine
rllvm-get-bc -m app                           # also write app.bc.manifest
```

The default strategy links modules into one `.bc`. `-b` is shorthand for archive
mode. Archive outputs are replaced with the current modules, so removed members
do not persist. `-m` writes contributing paths beside the input; use a catalog
when provenance or embedded bitcode archive members must be represented.

### Wrapper options and response files

Compiler flags, including `-c`, `-v`, `--help`, and `--version`, reach the real
compiler. Put wrapper options before them:

```text
--rllvm-compiler <PATH>   Override clang/clang++ (C/C++ wrappers only)
--rllvm-verbose[=LEVEL]   Bare flag is 1; 3 logs subcommands, 4 enables trace
--rllvm-help             Print wrapper help
--rllvm-version          Print wrapper version
```

```bash
rllvm-cc --rllvm-verbose=3 -pthread -c hello.c -ohello.o
rllvm-cc @compile.rsp
```

Use `=` for verbosity values; diagnostics go to stderr. Both `-o hello.o` and
`-ohello.o` work, and `--` remains supported for existing shims. C/C++ response
files follow Clang's GNU UTF-8 syntax, including quoting and nested references;
relative paths resolve from the compiler's working directory. Large generated
commands use response files automatically.

### WebAssembly and eBPF

```bash
rllvm-cc --target=wasm32-unknown-unknown -c lib.c -o lib.o
rllvm-cc --target=wasm32-unknown-unknown -c main.c -o main.o
rllvm-cc --target=wasm32-unknown-unknown -nostdlib -Wl,--no-entry \
  lib.o main.o -o app.wasm
rllvm-get-bc app.wasm -o app.bc

rllvm-cc --target=bpf -O2 -g -c prog.c -o prog.o
rllvm-get-bc prog.o -o prog.bc
```

WebAssembly linking needs a matching `wasm-ld` from LLD; see the
[WebAssembly example](examples/wasm/). BPF objects retain their native payload;
libbpf skips the extra `.rllvm_bc` section when loading and preserves it when
linking. `bpftool gen object` therefore retains contributing module paths.
That linker requires BTF, so compile with `-g`.

### Bitcode storage and relocation

C/C++ bitcode defaults to hidden files beside the requested output. Names
distinguish source, output, compiler, and settings. `bitcode_store_path` selects
an absolute directory for a central store. Preserve these files with the native
artifacts, including when restoring compiler-cache outputs.

Recorded paths are absolute by default. To move a build tree, record paths
relative to a common root and supply its new location during extraction:

```bash
RLLVM_BITCODE_ROOT="$PWD/build" cmake --build build
# After moving build/ to moved-build/:
rllvm-get-bc --bitcode-root moved-build moved-build/my_program
```

The root must contain the bitcode files, including a central store if used.
It changes recorded paths, not storage locations. Absolute and relative records
can coexist; `--bitcode-root` resolves only relative records.

### Bitcode caching

```bash
RLLVM_CACHE=1 cmake --build build   # a build already configured with the wrappers
```

The C/C++ cache is off by default. `RLLVM_CACHE=1` enables it; `0` disables it,
overriding `cache_enabled`. Storage defaults to `~/.rllvm/cache`; `cache_dir`
changes it.

Native compilation still runs. Hits avoid the extra bitcode compilation but
still preprocess current inputs and check dependencies, compiler, command,
directory, and environment. Unverifiable inputs generate uncached bitcode.
Disable caching for changing side inputs absent from preprocessing, such as
optimization profiles, and keep inputs stable during a build.

### LTO

Select `lto_mode` in the config or `RLLVM_LTO_MODE`:

| Mode | Captured bitcode | Support |
| --- | --- | --- |
| `marker` (default) | Per-source modules recorded in LTO objects | Full/ThinLTO; ELF/Mach-O; C/C++ |
| `save-temps` | Full-LTO linker's merged, optimized module | Separate links and combined source/link invocations |
| `skip` | No additional capture; emits a warning | Explicitly disabling capture for LTO |

```bash
RLLVM_LTO_MODE=save-temps rllvm-cc -flto hello.c -o hello
rllvm-get-bc hello -o hello.bc
```

Use the same mode when compiling and linking. `marker` supports mixed ordinary
and LTO objects, and records paths in both halves of fat-LTO objects.
`save-temps` needs actual LTO inputs; adding `-flto` only at link time is insufficient.
ThinLTO has no single merged module, so use `marker` for it.

Queries and non-linking invocations do not collect linker modules. User-requested
linker temporaries are preserved. COFF/WebAssembly reject `marker` and direct
users to `skip`. A `save-temps` link producing no merged module is an error.
Universal (multiple `-arch`) builds and combined universal binaries are unsupported;
build and extract one architecture at a time.

## Querying captured bitcode

Build with the optional `query` feature to ask nine source-level questions
about captured bitcode, from the command line or over MCP. It links LLVM via
`llvm-sys`, statically:

```bash
cargo install rllvm --features query
# When llvm-config is not on PATH, point at an LLVM 23 install:
LLVM_SYS_231_PREFIX=/opt/homebrew/opt/llvm cargo install rllvm --features query
```

`--catalog` takes JSON from `rllvm-get-bc --output-dir` or `rllvm-compdb
generate` (see Module catalogs and selection above):

```bash
rllvm-query --catalog catalog.json defs parse_frame            # every definition of the symbol
rllvm-query --catalog catalog.json at parser.c 4               # functions/call sites mapped to a source line
rllvm-query --catalog catalog.json callers parse_frame         # functions that call it
rllvm-query --catalog catalog.json callees main                # its outgoing call sites
rllvm-query --catalog catalog.json uses parse_frame            # non-call uses (address taken)
rllvm-query --catalog catalog.json reach main parse_frame       # one path from `main` to `parse_frame`
rllvm-query --catalog catalog.json closure parse_frame in       # functions that reach it (`out`: functions it reaches)
rllvm-query --catalog catalog.json externals                   # symbols the captured program leaves unbound
rllvm-query --catalog catalog.json indirect-targets parser.c:8 # the `!callees` bound at an indirect call site
```

Add `--heuristics` to include a heuristic address-taken inventory alongside
`indirect-targets`. Every answer is one JSON envelope: `results`, the catalog
`scope` it was computed over, an `analysis` of which modules actually parsed,
and an `uncertainty` block naming indirect call sites, functions without debug
locations, and ambiguous symbol bindings the answer could not see through.

Answers report whether the source behind a location has changed since capture
only when the catalog recorded a source hash. `rllvm-compdb generate` records
one; `rllvm-get-bc` and the other inventory paths do not, so their locations
report `source_status: "unknown"` rather than `current` or `modified`.

CLI subcommands are kebab-case (`indirect-targets`); MCP tool names are
snake_case (`indirect_targets`), matching `Query`'s own serde tag. The two
spellings coincide for every other query, which is one word either way.

### MCP server

```bash
rllvm-query --catalog catalog.json mcp
```

Serves the same nine queries as JSON-RPC 2.0 tools, newline-delimited over
stdio, and answers both the modern and legacy MCP protocol revisions. Point a
client at it:

```json
{
  "mcpServers": {
    "rllvm": {
      "command": "rllvm-query",
      "args": ["--catalog", "/path/to/catalog.json", "mcp"]
    }
  }
}
```

### Limitations

- An edge is a call present in the captured IR: `callers`, `callees`, `reach`,
  and `closure` see only calls the compiler emitted, not every call a running
  program could make.
- An empty `reach` is not unreachability. It says there is no path over
  resolved edges within the selected scope; `uncertainty` names the indirect
  sites and ambiguous bindings that could carry a path the walk cannot see.
- Cross-module resolution is by symbol name, which the catalog does not
  record. A unique binding is traversed and reported as the assumption it is;
  an ambiguous one stops the walk and is listed in `uncertainty.frontier`.
- Every definition a linker could pick is a binding candidate, and C++ emits
  many: a template instantiation, an inline member function or a defaulted
  constructor is emitted into every translation unit that uses it, so one such
  symbol is `ambiguous` with one candidate per module and `reach` stops there
  listing what are really copies of one function. The conservative direction --
  it halts a walk rather than inventing a path -- but it makes `reach` and
  `closure` less useful on C++ than on C until the extractor records LLVM's
  full linkage taxonomy.
- Locations are the source positions recorded at capture. If the file changed
  afterwards they no longer point where they did; `source_status` says so when
  the catalog recorded a source hash, and `unknown` when it did not.
- `!callees` at an indirect call site (`indirect-targets`) is an upper bound on
  the call's possible targets, not a reachable set. LLVM produces it only for
  patterns constant-value propagation (CVP) can bound, within one module; most
  indirect calls answer unresolved.
- The `--heuristics` address-taken inventory is opt-in and contributes no
  graph edges.
- One LLVM major per build: `rllvm-query` cannot read bitcode produced by a
  newer LLVM than the one it links.
- `-g -O0` is the supported analysis compilation: debug info locates results,
  and it is the configuration these queries are tested against.

## Configuration

The TOML file lives at `$RLLVM_CONFIG` or `~/.rllvm/config.toml`.
`rllvm-init --dry-run` previews detection; `--llvm-prefix` chooses a toolchain
and `-o` selects the file to write.

<details markdown="1">
<summary>Configuration keys</summary>

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


</details>

## How it works

C/C++ wrappers run Clang normally and also emit bitcode. Each object's custom
section records its bitcode path, newline-terminated. The linker combines those
sections; extraction reads the paths, deduplicates them, and merges or archives
the modules.

```text
source.c → rllvm-cc → object + bitcode
                         ↓
                      linker → executable
                                   ↓
                              rllvm-get-bc → whole-program.bc
```

Rust captures bitcode per crate. Linked crates carry paths through a marker
object; library crates carry them in archive members.

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

rllvm began as a Rust port of [gllvm](https://github.com/SRI-CSL/gllvm) and
[wllvm](https://github.com/SRI-CSL/whole-program-llvm), retaining the
`CC`/`CXX` → build → extract workflow. It adds Rust, WebAssembly/eBPF,
relocatable paths, catalogs, and selective compilation-database imports.

[rules_rllvm](https://github.com/h1994st/rules_rllvm) is a separate Bazel-native
project and does not use these binaries.

## License

[Apache-2.0](LICENSE)
