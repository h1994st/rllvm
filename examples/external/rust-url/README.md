# rust-url + rllvm

[rust-url](https://github.com/servo/rust-url) is the `url` crate and the
`idna` and `percent-encoding` crates it builds on, in one Cargo workspace. Its
cargo-fuzz targets live in `url/fuzz`; this builds the `parse` one through
rllvm and follows a path from libFuzzer's entry point into `idna`.

No `check.sh` — see [external/](../README.md).
[`reproduce.sh`](reproduce.sh) runs every flow below on a macOS host and prints
the numbers under [Validated against](#validated-against).

## Build and extract

```bash
rustup toolchain install nightly
cargo install cargo-fuzz

git clone --branch v2.5.8 --depth 1 https://github.com/servo/rust-url && cd rust-url/url

RUSTC_WRAPPER=rllvm-rustc cargo +nightly fuzz build parse

rllvm-get-bc fuzz/target/aarch64-apple-darwin/release/parse -o parse.bc
rllvm-info parse.bc
```

The target directory is named after the host triple; on Linux it is
`x86_64-unknown-linux-gnu`. The bitcode holds one module per crate the fuzzer
compiles: 13, from `url`, `idna` and `percent_encoding` through the `icu_*`
crates and `libfuzzer-sys`.

## What ends up captured

Every function the fuzz binary defines is in the bitcode except those cargo
never compiles from a crate:

- the prebuilt standard library: `core`, `std`, `alloc`, and the crates `std`
  itself depends on, such as `gimli` and `rustc_demangle`;
- libFuzzer, C++ that `libfuzzer-sys` compiles through the `cc` crate: its
  `main`, the `__sanitizer_cov_*` callbacks it defines, and the libc++
  templates it instantiates;
- `OUTLINED_FUNCTION_*`, which the code generator creates after the bitcode is
  written.

cargo-fuzz instruments the build for AddressSanitizer and SanitizerCoverage,
and rllvm captures the IR after that instrumentation, so answers show it:

```bash
rllvm-get-bc fuzz/target/aarch64-apple-darwin/release/parse --output-dir cat
rllvm-query --catalog cat/catalog.json callees idna::domain_to_ascii_from_cow
```

```text
<no location>  direct    __sanitizer_cov_trace_const_cmp4
<no location>  direct    __asan_stack_malloc_1
<no location>  direct    __sanitizer_cov_trace_const_cmp8
src/lib.rs:136  direct    <idna::uts46::Uts46>::to_ascii_from_cow
src/lib.rs:142  direct    __sanitizer_cov_trace_const_cmp8

note: 1069 function(s) without a source location
```

`cargo fuzz build -s none parse` drops AddressSanitizer; the coverage hooks
stay, because libFuzzer needs them.

## From the fuzzer into idna

```bash
rllvm-query --catalog cat/catalog.json reach rust_fuzzer_test_input idna::domain_to_ascii_from_cow
```

```text
call              rust_fuzzer_test_input
call              parse::_::__libfuzzer_sys_run
binding           <url::ParseOptions>::parse  (unique, 1 candidate(s))
call              <url::ParseOptions>::parse
call              <url::parser::Parser>::parse_file
call              <url::host::Host<alloc::borrow::Cow<str>>>::parse_cow
binding           idna::domain_to_ascii_from_cow  (unique, 1 candidate(s))

note: 51 indirect call site(s), 1 with an LLVM target bound
```

The path runs from `libfuzzer-sys` through the fuzz target and `url` into
`idna`, each a separate crate and module. `reach` returns one supporting path:
here a `file:` URL with a host, one of several ways a host reaches `idna`.

## Without cargo-fuzz

A plain build of the crate works the same way. `url` is a workspace member, so
its artifacts land in the workspace's `target/`, not `url/target/`:

```bash
cd .. && RUSTC_WRAPPER=rllvm-rustc cargo build --release -p url
rllvm-get-bc target/release/liburl.rlib -o url.bc
```

rustc splits a release crate across several codegen units, and rllvm links
them into one module per crate. With `RUSTFLAGS=-Copt-level=0` nothing is
inlined, so the module keeps 788 functions instead of 146; that build needs
rllvm 0.7.1 or newer.

## Validated against

rust-url `v2.5.8` (`d6ea13c`) on arm64 macOS with rustc 1.100.0-nightly (LLVM
23.1.1) and cargo-fuzz 0.13.2, extracted with LLVM 23.1.2. `parse.bc` holds 13
modules and 719 functions.

rust-url commits no `Cargo.lock`, so dependency versions resolve when you
build; this run resolved `libfuzzer-sys` 0.4.13 and the `icu_*` crates at
2.3.
