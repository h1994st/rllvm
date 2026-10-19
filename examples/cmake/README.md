# CMake + rllvm Example

The toolchain file `cmake/rllvm-toolchain.cmake` sets all four compilers: C,
C++, Objective-C and Objective-C++. Setting `CC=rllvm-cc CXX=rllvm-cxx` before
`cmake` works too.

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
