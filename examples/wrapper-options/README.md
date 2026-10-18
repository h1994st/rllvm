# Wrapper options + rllvm Example

Wrapper options are long-only, prefixed `--rllvm-`, and come first. Every other
argument reaches the real compiler untouched, including `-c`, `-v`, `--help`,
`--version` and `@file` response files.

```text
--rllvm-compiler <PATH>   Override clang/clang++ (C/C++ wrappers only)
--rllvm-verbose[=LEVEL]   Bare flag is 1; 3 logs subcommands, 4 enables trace
--rllvm-help              Print wrapper help
--rllvm-version           Print wrapper version
```

Response files follow Clang's GNU UTF-8 syntax, including quoting and nested
references. Large generated commands use them automatically.

## Build and verify

```bash
./check.sh
```

It writes a response file, compiles through it with `--rllvm-verbose=3`, and
checks two things: the bitcode defines `twice`, so the response file really was
expanded; and stdout is empty, because diagnostics go to stderr — build systems
read compiler stdout.

## What it does

```bash
printf -- '-c\ndemo.c\n-o\nbuild/demo.o\n' > build/args.rsp
rllvm-cc --rllvm-verbose=3 @build/args.rsp
rllvm-get-bc build/demo.o -o build/demo.bc
```

## Inspect the result

```bash
llvm-nm --defined-only build/demo.bc
rllvm-cc --rllvm-help
```
