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
#
# Objective-C and Objective-C++ go through the same two wrappers. CMake only
# hands OBJC down from the C compiler, and OBJCXX from the C++ one, when those
# languages are enabled too, so both are named here: a project enabling OBJC
# alone would otherwise silently fall back to the system clang.

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
set(CMAKE_OBJC_COMPILER "${RLLVM_CC}")
set(CMAKE_OBJCXX_COMPILER "${RLLVM_CXX}")
