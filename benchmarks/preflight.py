"""Check prepared identities against current inputs before measured work."""

import os
from collections.abc import Callable
from pathlib import Path
from typing import Any

from benchmarks.fixtures import FixtureError, PreparedFixture
from benchmarks.process import Command, Measurement
from benchmarks.toolchains import Toolchain, sha256, version_arguments


def verify_prepared(
    prepared: PreparedFixture,
    tools: Toolchain,
    execute: Callable[[Command], Measurement],
    observations: list[dict[str, Any]],
) -> None:
    """Validate saved identities without replacing their pins."""

    def compare(identity: str, expected: Any, observed: Any) -> None:
        observations.append(
            {"identity": identity, "expected": expected, "observed": observed}
        )
        if observed != expected:
            raise FixtureError(f"prepared {identity} differs")

    def command(cwd: Path, *argv: str) -> str:
        result = execute(Command(argv, cwd, dict(tools.environment)))
        return (
            Path(result.stdout).read_text() + Path(result.stderr).read_text()
        ).strip()

    for name, tool in tools.tools.items():
        path = Path(tool.path)
        compare(f"tool {name} executable", True, os.access(path, os.X_OK))
        compare(f"tool {name} realpath", tool.realpath, str(path.resolve()))
        compare(f"tool {name} SHA256", tool.sha256, sha256(path))
        compare(
            f"tool {name} version",
            tool.version,
            command(prepared.source, tool.path, *version_arguments(name)),
        )

    def git(cwd: Path, *args: str) -> str:
        return command(cwd, tools.path("git"), "--no-optional-locks", *args)

    def clean(cwd: Path, *, copied_lock: bool = False) -> None:
        paths = (".", ":(exclude)Cargo.lock") if copied_lock else (".",)
        # Check index and worktree separately: opposing staged/unstaged edits
        # must not cancel each other out in a diff against HEAD.
        for index in ((), ("--cached",)):
            git(
                cwd,
                "diff",
                "--no-ext-diff",
                "--exit-code",
                "--ignore-submodules=untracked",
                *index,
                "--",
                *paths,
            )

    compare(
        "source HEAD",
        prepared.commit,
        git(prepared.source, "rev-parse", "HEAD"),
    )
    compare(
        "source tree",
        prepared.tree,
        git(prepared.source, "rev-parse", "HEAD^{tree}"),
    )
    if prepared.lock_sha256 is not None:
        compare(
            "Cargo.lock SHA256",
            prepared.lock_sha256,
            sha256(prepared.source / "Cargo.lock"),
        )
    clean(prepared.source, copied_lock=prepared.lock_sha256 is not None)
    compare(
        "required submodules",
        sorted(prepared.recipe.required_submodules),
        sorted(prepared.submodules),
    )
    for relative, pin in prepared.submodules.items():
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts or not path.parts:
            raise FixtureError(f"unsafe prepared submodule path: {relative}")
        source = prepared.source / path
        if source.is_symlink():
            raise FixtureError(f"prepared submodule is a symlink: {relative}")
        compare(
            f"submodule {relative} gitlink",
            f"160000 commit {pin}\t{relative}",
            git(prepared.source, "ls-tree", "HEAD", "--", relative),
        )
        compare(
            f"submodule {relative} HEAD", pin, git(source, "rev-parse", "HEAD")
        )
        clean(source)
