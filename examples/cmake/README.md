# CMake + rllvm Example

This example demonstrates using rllvm with a CMake project via the toolchain file.

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

`./check.sh` runs the build and the extraction above, then checks that the
bitcode defines `main`. `cargo test --test examples` runs it in CI.
