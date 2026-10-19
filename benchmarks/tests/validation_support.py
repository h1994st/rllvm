"""Small real-tool correctness fixtures, never performance measurements."""

import json
import os
import shutil
import subprocess
from dataclasses import replace
from pathlib import Path
from typing import Any, cast

from benchmarks.fixtures import PreparedFixture
from benchmarks.process import Command, checked
from benchmarks.recipes import Edit, Recipe
from benchmarks.records import read_records as _read_records
from benchmarks.toolchains import Toolchain, child_environment, sha256

FIXTURES = Path(__file__).parent / "fixtures"
ROOT = Path(__file__).resolve().parents[2]


def llvm_tool_paths() -> dict[str, Path]:
    llvm_config = shutil.which("llvm-config")
    if llvm_config is None:
        raise RuntimeError("llvm-config is required for benchmark tests")
    bindir = Path(
        subprocess.check_output((llvm_config, "--bindir"), text=True).strip()
    )
    names = (
        "clang",
        "clang++",
        "llvm-ar",
        "llvm-ranlib",
        "llvm-link",
        "llvm-dis",
        "llvm-nm",
        "llvm-objcopy",
        "opt",
    )
    paths = {name: bindir / name for name in names}
    paths["llvm-config"] = Path(llvm_config)
    missing = tuple(name for name, path in paths.items() if not path.is_file())
    if missing:
        raise RuntimeError(
            "llvm-config tool directory is incomplete: " + ", ".join(missing)
        )
    return paths


def tools_at(root):
    names = (
        "git",
        "clang",
        "clang++",
        "llvm-ar",
        "llvm-link",
        "llvm-dis",
        "llvm-nm",
        "llvm-config",
        "llvm-objcopy",
        "opt",
        "cmake",
        "ninja",
        "cargo",
        "rustc",
        "rllvm-cc",
        "rllvm-cxx",
        "rllvm-rustc",
        "rllvm-get-bc",
    )
    paths = llvm_tool_paths()
    paths.update(
        {
            name: ROOT
            / os.environ.get("RLLVM_BENCH_TEST_BIN_DIR", "target/release")
            / name
            for name in names
            if name.startswith("rllvm-")
        }
    )
    return Toolchain.discover(names, root / "tool-logs", paths=paths)


def environment(root, tools):
    env = child_environment(os.environ)
    config = root / "rllvm.toml"
    keys = {
        "clang": "clang",
        "clang++": "clangxx",
        "rustc": "rustc",
        "llvm-ar": "llvm_ar",
        "llvm-link": "llvm_link",
        "llvm-config": "llvm_config",
        "llvm-objcopy": "llvm_objcopy",
    }
    config.write_text(
        "\n".join(
            f"{key}_filepath = {json.dumps(tools.path(name))}"
            for name, key in keys.items()
        )
        + f"\nbitcode_store_path = {json.dumps(str(root / 'bitcode'))}\n"
    )
    env.update(RLLVM_CONFIG=str(config), RLLVM_CACHE="0")
    return env


def run(argv, root, env, label):
    return checked(
        Command(tuple(map(str, argv)), root, env), root / "logs", label
    )


def read_records(path: Path) -> list[dict[str, Any]]:
    return cast(list[dict[str, Any]], _read_records(path))


def commit_fixture(prepared: PreparedFixture) -> PreparedFixture:
    """Prepare intentional test inputs with honest current Git identities."""

    def git(*args):
        return subprocess.check_output(
            (prepared.toolchain.path("git"), *args),
            cwd=prepared.source,
            env=dict(
                prepared.toolchain.environment, GIT_CONFIG_GLOBAL=os.devnull
            ),
            stderr=subprocess.PIPE,
            text=True,
        ).strip()

    if not (prepared.source / ".git").exists():
        git("init", "-q")
    git("config", "user.name", "Fixture")
    git("config", "user.email", "fixture@example.invalid")
    git("add", ".")
    git("commit", "--allow-empty", "-qm", "fixture inputs")
    commit = git("rev-parse", "HEAD")
    lock = prepared.source / "Cargo.lock"
    return replace(
        prepared,
        identity="fixture-" + commit,
        commit=commit,
        tree=git("rev-parse", "HEAD^{tree}"),
        recipe=replace(prepared.recipe, commit=commit),
        lock_sha256=sha256(lock) if lock.is_file() else None,
    )


def tiny_cmake_fixture(root: Path, *, spaces: bool = False) -> PreparedFixture:
    """A committed tiny C/C++ CMake fixture whose children record probes."""
    root.mkdir()
    discovered = tools_at(root)
    tools = Toolchain.discover(
        ("llvm-ranlib", "rllvm-info"),
        root / ("extra tools" if spaces else "extra-tools"),
        paths={
            "llvm-ranlib": Path(discovered.path("llvm-ar")).with_name(
                "llvm-ranlib"
            ),
            "rllvm-info": Path(discovered.path("rllvm-cc")).with_name(
                "rllvm-info"
            ),
        },
    )
    tools = replace(discovered, tools=discovered.tools | tools.tools)
    source = root / ("source with spaces" if spaces else "source")
    (source / "lib/includes/tiny").mkdir(parents=True)
    (source / "tests").mkdir()
    (source / "lib/includes/tiny/tiny.h").write_text(
        "struct tiny_info { const char *version_str; };\n"
        "const struct tiny_info *tiny_version(int);\n"
    )
    (source / "lib/version.c").write_text(
        '#include "tiny/tiny.h"\n'
        'static const struct tiny_info info = {"version"};\n'
        "const struct tiny_info *tiny_version(int unused){return &info;}\n"
    )
    (source / "tests/main.cpp").write_text(
        '#include <cstdio>\nextern "C" {\n#include "tiny/tiny.h"\n}\n'
        'int main(){puts(tiny_version(0)->version_str);puts("/tiny [ OK ]\\n'
        '1 of 1 (100%) tests successful, 0 (0%) test skipped.");return 0;}\n'
    )
    (source / "CMakeLists.txt").write_text(
        "cmake_minimum_required(VERSION 3.20)\n"
        "project(tiny LANGUAGES C CXX)\n"
        "include_directories(lib/includes)\n"
        "add_library(tiny SHARED lib/version.c)\n"
        "add_library(tiny_static STATIC lib/version.c)\n"
        "set_target_properties(tiny tiny_static PROPERTIES "
        "OUTPUT_NAME tiny ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib "
        "LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)\n"
        "add_executable(main tests/main.cpp)\n"
        "target_link_libraries(main PRIVATE tiny)\n"
        "set_target_properties(main PROPERTIES "
        "RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/tests)\n"
        "execute_process(COMMAND ${CMAKE_COMMAND} -E environment "
        "OUTPUT_FILE ${CMAKE_BINARY_DIR}/child-environment)\n"
        "file(WRITE ${CMAKE_BINARY_DIR}/child-cwd ${CMAKE_BINARY_DIR})\n"
    )
    recipe = Recipe(
        "tiny-c-cmake",
        "tiny",
        "cmake",
        "local-fixture",
        "fixture",
        (),
        False,
        (),
        (),
        ("tiny", "tiny_static", "main"),
        "static",
        Edit("lib/version.c", '"version"', '"version-rllvm-benchmark"'),
    )
    return commit_fixture(
        PreparedFixture(
            "tiny",
            source,
            recipe,
            "fixture",
            "fixture",
            {},
            None,
            tools,
            (),
            root / ("prepared fixture.json" if spaces else "fixture.json"),
        )
    )
