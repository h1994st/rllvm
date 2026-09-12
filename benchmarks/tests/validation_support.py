"""Small real-tool correctness fixtures, never performance measurements."""

import json
import os
from pathlib import Path

from benchmarks.process import Command, checked
from benchmarks.toolchains import Toolchain, child_environment

FIXTURES = Path(__file__).parent / "fixtures"
ROOT = Path(__file__).resolve().parents[2]


def tools_at(root):
    names = (
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
    return Toolchain.discover(
        names,
        root / "tool-logs",
        paths={
            name: ROOT / "target/release" / name
            for name in names
            if name.startswith("rllvm-")
        },
    )


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
