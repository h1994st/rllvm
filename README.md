# Whole Program LLVM in Rust (rllvm)

[![rllvm CI](https://github.com/h1994st/rllvm/actions/workflows/ci.yml/badge.svg)](https://github.com/h1994st/rllvm/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/h1994st/rllvm/graph/badge.svg?token=PWKZ6H64BS)](https://codecov.io/github/h1994st/rllvm)

`rllvm` is a Rust port of [`gllvm`](https://github.com/SRI-CSL/gllvm) and provides compiler wrappers to build whole-program LLVM bitcode files for projects with source codes.

For more details, please refer to [`gllvm`](https://github.com/SRI-CSL/gllvm) or [`wllvm`](https://github.com/SRI-CSL/whole-program-llvm).

## Installation

```bash
cargo install rllvm
```

## Get Started

```bash
# Compile the source file. `--` is needed. Arguments after it will be
# passed to the wrapped compiler
rllvm-cxx -- -o hello tests/data/hello.cc

# Extract the bitcode file `hello.bc`
rllvm-get-bc hello

# Obtain readable `hello.ll` file
llvm-dis hello.bc
```

### Configuration

Users can specify the configuration file by setting the environment variable `RLLVM_CONFIG`.

```bash
export RLLVM_CONFIG=/absolute/path/to/config/file.toml
```

Otherwise, the default configuration file `~/.rllvm/config.toml` will be used. The configuration file will be automatically created, if it does not exist, with the following entries:

| Configuration Key          | Required? | Notes                                                                                   |
| -------------------------- | --------- | --------------------------------------------------------------------------------------- |
| `llvm_config_filepath`     | Yes       | The absolute filepath of `llvm-config`                                                  |
| `clang_filepath`           | Yes       | The absolute filepath of `clang`                                                        |
| `clangxx_filepath`         | Yes       | The absolute filepath of `clang++`                                                      |
| `llvm_ar_filepath`         | Yes       | The absolute filepath of `llvm-ar`                                                      |
| `llvm_link_filepath`       | Yes       | The absolute filepath of `llvm-link`                                                    |
| `llvm_objcopy_filepath`    | Yes       | The absolute filepath of `llvm-objcopy`                                                 |
| `bitcode_store_path`       | No        | The absolute path of the directory that stores intermediate bitcode files               |
| `llvm_link_flags`          | No        | Extra user-provided linking flags for `llvm-link`                                       |
| `lto_ldflags`              | No        | Extra user-provided linking flags for link time optimization                            |
| `bitcode_generation_flags` | No        | Extra user-provided flags for bitcode generation, e.g., "-flto -fwhole-program-vtables" |
| `is_configure_only`        | No        | The configure only mode, which skips the bitcode generation (Default: false)            |
| `log_level`                | No        | Log level (0: nothing, 1: error, 2: warn, 3: info, 4: debug, 5: trace)                  |

Here is an example of the configuration file:

```toml
llvm_config_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/llvm-config'
clang_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/clang'
clangxx_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/clang++'
llvm_ar_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/llvm-ar'
llvm_link_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/llvm-link'
llvm_objcopy_filepath = '/usr/local/Cellar/llvm/16.0.4/bin/llvm-objcopy'
bitcode_store_path = '/tmp/bitcode_store'
log_level = 3
```

Furthermore, we can override configurations by providing extra arguments before `--`.
Currently, we only support override the compiler path and the log level, which is useful to sepcify relative paths while working with Bazel.

```bash
# Override the wrapped compiler path and show all logs
rllvm-cxx -c /path/to/compiler -vvvvv -- -o hello tests/data/hello.cc
```
