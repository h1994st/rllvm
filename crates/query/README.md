# rllvm-query

[![crates.io](https://img.shields.io/crates/v/rllvm-query.svg)](https://crates.io/crates/rllvm-query)

Source-level questions about bitcode captured by
[rllvm](https://crates.io/crates/rllvm): where a symbol is defined, who calls
it, what it reaches, and what the captured program's boundary is. Also serves
the same ten queries over MCP stdio for an agent to use.

```bash
cargo install rllvm-query   # needs LLVM_SYS_231_PREFIX if llvm-config is not on PATH
rllvm-query --catalog build/catalog.json callers helper
```

Point it at a catalog from `rllvm-get-bc` or `rllvm-compdb generate`. See the
[repository README](https://github.com/h1994st/rllvm#readme) for how to capture
one, and `examples/queries/` and `examples/mcp/` for worked flows.

This is the only rllvm crate that links LLVM, through `llvm-sys`. Capturing
bitcode does not need it, so the wrappers do not depend on this crate.

## Answers never claim more than they know

`scope` is quoted from the catalog and never shrinks; what was actually read
is reported separately under `analysis`. Indirect call sites appear as
unresolved rather than being dropped, an empty `reach` is not unreachability,
and `!callees` is an upper bound, not a reachable set.

## License

[Apache-2.0](../../LICENSE)
