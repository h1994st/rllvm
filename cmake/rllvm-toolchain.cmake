# rllvm CMake toolchain file
#
# Usage:
#   cmake -DCMAKE_TOOLCHAIN_FILE=path/to/rllvm-toolchain.cmake ..
#
# This toolchain file configures CMake to use rllvm's compiler wrappers
# (rllvm-cc, rllvm-cxx) instead of the default system compilers. These
# wrappers transparently run clang/clang++ while simultaneously generating
# LLVM bitcode files. After building, use rllvm-get-bc to extract
# whole-program bitcode from the resulting binaries.

find_program(RLLVM_CC rllvm-cc)
find_program(RLLVM_CXX rllvm-cxx)

if(NOT RLLVM_CC)
    message(FATAL_ERROR "rllvm-cc not found. Install rllvm: cargo install rllvm")
endif()

if(NOT RLLVM_CXX)
    message(FATAL_ERROR "rllvm-cxx not found. Install rllvm: cargo install rllvm")
endif()

set(CMAKE_C_COMPILER "${RLLVM_CC}")
set(CMAKE_CXX_COMPILER "${RLLVM_CXX}")
