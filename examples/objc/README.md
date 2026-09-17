# Objective-C + rllvm Example

Objective-C (`.m`) and Objective-C++ (`.mm`) go through `rllvm-cc` and
`rllvm-cxx`, the same wrappers as C and C++.

This project enables `OBJC` on its own, which is the case the toolchain file
has to name explicitly: with no C language enabled, CMake has no C compiler to
hand down from.

## Requirements

macOS. The example links `-framework Foundation`; on Linux you need GNUstep and
its own flags instead.

## Build

```bash
cmake -B build -DCMAKE_TOOLCHAIN_FILE=../../cmake/rllvm-toolchain.cmake
cmake --build build
```

## Extract bitcode

```bash
rllvm-get-bc build/hello -o build/hello.bc
```

## Inspect the result

```bash
llvm-dis -o - build/hello.bc | grep '^define'
```
