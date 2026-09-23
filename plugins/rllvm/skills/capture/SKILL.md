---
name: capture
description: Capture LLVM bitcode from a project with rllvm and turn it into a catalog or module. Use when the user wants bitcode, a whole-program .bc, a catalog, or to analyse or query code that has not been captured yet — C, C++, Objective-C, Rust, mixed-language, cross-compiled, WebAssembly or eBPF.
---

# Capturing bitcode

Run the build as an ordinary shell command the user can see and approve —
never through an MCP tool. A full build can take minutes; say so before
starting. If rllvm is not set up, use the `setup` skill first.

## 1. How is it built?

| Build | Capture |
| --- | --- |
| C / C++, make or autotools | `CC=rllvm-cc CXX=rllvm-cxx ./configure && make` |
| CMake | `CC=rllvm-cc CXX=rllvm-cxx cmake -S . -B build && cmake --build build` |
| Objective-C / Objective-C++ | also `OBJC=rllvm-cc OBJCXX=rllvm-cxx` |
| Cargo | `RUSTC_WRAPPER=rllvm-rustc cargo build` |
| One Rust file | `rllvm-rustc main.rs -o app` |
| Mixed C/C++ and Rust | `CC=rllvm-cc CXX=rllvm-cxx RUSTC_WRAPPER=rllvm-rustc cargo build` ([ffi example](https://github.com/h1994st/rllvm/tree/main/examples/ffi)) |
| Only `compile_commands.json`, no rebuild wanted | `rllvm-compdb generate build/ --output-dir DIR` (narrow with `--source` or `--entry`; only direct `clang`/`clang++` drivers are supported) |
| A hand-written command or `@response` file | the wrappers take the same arguments |

A fresh build directory avoids reusing objects compiled without the wrappers.
`rllvm-compdb` describes the current source tree, not what a real link
contained; prefer wrapper capture when that matters. `cargo check` and
procedural-macro crates are not captured, and the Rust standard library is not
rebuilt.

## 2. What needs care

Compiler flags reach the real compiler unchanged. These need a decision:

- **LTO.** `RLLVM_LTO_MODE`: `marker` (default), `save-temps` (full LTO's
  merged module), or `skip`; use the same mode for compile and link. An
  archive of `-flto` objects, full or thin, holds bitcode, not objects, and
  cannot be extracted — turn LTO off to extract a library. A linked
  executable still extracts either way.
- **Cross-compilation.** `--target=<triple>` as for Clang. Linking ELF from a
  non-ELF host needs LLD (`-fuse-ld=lld`). RISC-V and other targets the
  internal fallback does not model need `llvm_objcopy_filepath` in the config.
- **WebAssembly** needs a matching `wasm-ld`. **eBPF** needs `-g`.
- **Universal** (several `-arch`) builds are unsupported: one architecture at a
  time.
- **A tree that will move:** build with `RLLVM_BITCODE_ROOT="$PWD/build"`, then
  extract with `--bitcode-root <new location>`.
- **Rebuilds:** `RLLVM_CACHE=1` reuses bitcode after validating inputs; leave
  it off when side inputs such as optimisation profiles change.

## 3. What to extract

`rllvm-get-bc` takes executables, objects, archives and `.rlib`s.

| Want | Command |
| --- | --- |
| Whole program | `rllvm-get-bc app -o app.bc` |
| Look before extracting | `rllvm-info app` (`--json` lists every module) |
| A library as an archive of modules | `rllvm-get-bc --merge-strategy archive libfoo.a` |
| A subset, as a catalog | `rllvm-get-bc app --source src/x.c --output-dir DIR` (also `--module`, `--configuration`) |

`--output-dir` must be new; it writes `DIR/catalog.json`.

**Handing a module to another tool** (PhASAR, SVF, KLEE, `opt`): extract one
module with `-o x.bc`, never an archive (`-b`); capture with an LLVM no newer
than the tool's; turn LTO off to extract a single library.

## 4. Query it

Call `load_catalog` with `DIR/catalog.json`, or `inventory` with the built
artifact when no catalog was written. After a rebuild, load the same path again
to replace it. Then use the `query` skill.

## When nothing was captured

Rerun one compile with `--rllvm-verbose=3` (under Cargo,
`RLLVM_LOG_LEVEL=3`) and read what the wrapper did before changing anything.
The `setup` skill's troubleshooting table covers the common causes.

Details: the rllvm README, "Capturing bitcode" and "Extracting bitcode".
