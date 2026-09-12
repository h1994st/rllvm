"""Small real-tool correctness fixtures, never performance measurements."""

import json
import os
import shutil
import subprocess
from dataclasses import replace
from pathlib import Path

from benchmarks.fixtures import PreparedFixture
from benchmarks.process import Command, checked
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
