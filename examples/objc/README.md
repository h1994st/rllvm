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

## Verify

`./check.sh` runs the build and the extraction above, then checks that
`-[Greeter greet:]` reached the bitcode — an Objective-C method rather than
`main`, so a `.m` built by the system compiler cannot pass. `cargo test
--test examples` runs it in CI.
