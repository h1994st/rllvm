# CMake + rllvm Example

This example demonstrates using rllvm with a CMake project via the toolchain file.

## Build

```bash
cmake -B build -DCMAKE_TOOLCHAIN_FILE=../../cmake/rllvm-toolchain.cmake
cmake --build build
```

## Extract bitcode

```bash
rllvm-get-bc build/hello    # produces build/hello.bc
```
