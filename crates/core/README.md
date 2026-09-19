# rllvm-core

[![crates.io](https://img.shields.io/crates/v/rllvm-core.svg)](https://crates.io/crates/rllvm-core)

The library behind the [rllvm](https://crates.io/crates/rllvm) compiler
wrappers: argument classification, bitcode capture, and catalogs.

Most users want [`rllvm`](https://crates.io/crates/rllvm) — the `rllvm-cc`,
`rllvm-cxx`, `rllvm-rustc`, `rllvm-get-bc`, and other command-line tools — not
this crate directly. See the
[repository README](https://github.com/h1994st/rllvm#readme) for what rllvm
does and how to use it.

## Who depends on this directly

Someone building a new compiler wrapper. `rllvm-core` provides:

- `arg_parser`, which classifies compiler command lines.
- `compiler_wrapper::CompilerWrapper`, the trait a wrapper implements, and
  `compiler_wrapper::llvm`, the writers that record a bitcode path into an
  object's own section so linkers can carry it through to the finished binary.
- `catalog`, the versioned module-catalog format shared by capture and
  extraction.
- `cache`, `config`, `lto`, and `merge`, the supporting pieces those wrappers
  need.

`rllvm` (the wrapper binaries) is built on this crate.

## License

[Apache-2.0](../../LICENSE)
