cmake_minimum_required(VERSION 3.16)
project(rllvm_objc_example LANGUAGES OBJC)

add_executable(hello hello.m)
target_link_libraries(hello "-framework Foundation")
