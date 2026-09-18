# eBPF + rllvm Example

eBPF works without special handling. Each object records its bitcode path in a
custom section; libbpf skips that section on load, and the linker preserves it.

## Requirements

clang with the `bpf` target, which standard LLVM builds include:

```bash
clang --print-targets | grep bpf
```

## Build and verify

```bash
./check.sh
```

It compiles the program for `bpf` and checks the extracted bitcode defines
`count_packet`.

## What it does

```bash
rllvm-cc --target=bpf -O2 -g -c prog.c -o build/prog.o
rllvm-get-bc build/prog.o -o build/prog.bc
```

`-g` is required: the BPF linker needs BTF.

## Inspect the result

```bash
llvm-objdump --section-headers build/prog.o | grep rllvm_bc
llvm-dis -o - build/prog.bc | grep '^define'
```
