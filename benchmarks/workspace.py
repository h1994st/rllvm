"""Owned benchmark workspaces and cross-process run locking."""

import errno
import fcntl
import os
import shutil
import tempfile
from dataclasses import dataclass
from pathlib import Path
from types import TracebackType

from benchmarks.records import (
    SCHEMA_VERSION,
    RecordError,
    read_json,
    write_json,
)

_MARKER_NAME = ".rllvm-workflow-workspace.json"
_MARKER_KIND = "rllvm-workflow-benchmark"


class WorkspaceError(RuntimeError):
    """A path is not a verified benchmark-owned workspace or descendant."""


class RunLockError(RuntimeError):
    """Another workflow benchmark holds the host-wide run lock."""


class RunLock:
    """An advisory per-user lock whose inode remains stable after release."""

    def __init__(self, path: Path | None = None) -> None:
        if path is None:
            path = Path(tempfile.gettempdir()) / (
                f"rllvm-workflow-benchmark-{os.getuid()}.lock"
            )
        self.path = path
        self._descriptor: int | None = None

    def __enter__(self) -> RunLock:
        if self._descriptor is not None:
            raise RunLockError("run lock instance is already held")
        self.path.parent.mkdir(parents=True, exist_ok=True)
        descriptor = os.open(self.path, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            os.close(descriptor)
            if error.errno in {errno.EACCES, errno.EAGAIN}:
                raise RunLockError(
                    f"another benchmark holds run lock {self.path}"
                ) from error
            raise

        try:
            state = f"pid={os.getpid()}\n".encode()
            os.ftruncate(descriptor, 0)
            os.write(descriptor, state)
            os.fsync(descriptor)
        except BaseException:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            finally:
                os.close(descriptor)
            raise
        self._descriptor = descriptor
        return self

    def __exit__(
        self,
        exception_type: type[BaseException] | None,
        exception: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        descriptor = self._descriptor
        self._descriptor = None
        if descriptor is None:
            return
        try:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
        finally:
            os.close(descriptor)


@dataclass(frozen=True)
class Workspace:
    root: Path
    marker: Path

    @classmethod
    def create(cls, root: Path) -> Workspace:
        """Create a workspace or reopen one with a valid ownership marker."""
        root = root.absolute()
        if root.is_symlink():
            raise WorkspaceError(
                f"workspace root may not be a symlink: {root}"
            )

        marker = root / _MARKER_NAME
        workspace = cls(root=root, marker=marker)
        if root.exists():
            workspace._verify()
            return workspace

        root.mkdir(parents=True)
        try:
            write_json(
                marker,
                {
                    "schema_version": SCHEMA_VERSION,
                    "kind": _MARKER_KIND,
                    "root": str(root.resolve()),
                },
            )
        except BaseException:
            root.rmdir()
            raise
        workspace._verify()
        return workspace

    def reset_directory(self, relative: str | Path) -> Path:
        """Replace a named, verified descendant with an empty directory."""
        self._verify()
        target = self._descendant(relative)
        if target.is_symlink():
            target.unlink()
        elif target.exists():
            if not target.is_dir():
                raise WorkspaceError(
                    f"owned directory path is not a directory: {target}"
                )
            shutil.rmtree(target)
        target.mkdir(parents=True)
        return target

    def reset_file(self, relative: str | Path) -> Path:
        """Remove a named file descendant and return its owned path."""
        self._verify()
        target = self._descendant(relative)
        if target.is_symlink() or target.is_file():
            target.unlink()
        elif target.exists():
            raise WorkspaceError(f"owned file path is a directory: {target}")
        target.parent.mkdir(parents=True, exist_ok=True)
        return target

    def _verify(self) -> None:
        if self.root.is_symlink() or not self.root.is_dir():
            raise WorkspaceError(
                f"workspace root is not an owned directory: {self.root}"
            )
        expected_marker = self.root / _MARKER_NAME
        if self.marker != expected_marker:
            raise WorkspaceError(
                f"workspace ownership marker is outside its root: {self.marker}"
            )
        if self.marker.is_symlink() or not self.marker.is_file():
            raise WorkspaceError(
                f"workspace ownership marker is missing: {self.marker}"
            )
        try:
            value = read_json(self.marker)
        except (OSError, RecordError) as error:
            raise WorkspaceError(
                f"workspace ownership marker is invalid: {self.marker}"
            ) from error
        if value.get("kind") != _MARKER_KIND:
            raise WorkspaceError(
                f"workspace ownership marker has wrong kind: {self.marker}"
            )
        if value.get("root") != str(self.root.resolve()):
            raise WorkspaceError(
                f"workspace ownership marker has wrong root: {self.marker}"
            )

    def _descendant(self, relative: str | Path) -> Path:
        relative = Path(relative)
        if (
            relative.is_absolute()
            or relative in {Path(), Path(".")}
            or ".." in relative.parts
        ):
            raise WorkspaceError(
                f"owned path must be a named relative descendant: {relative}"
            )
        target = self.root / relative
        root = self.root.resolve()
        resolved_parent = target.parent.resolve()
        if resolved_parent != root and root not in resolved_parent.parents:
            raise WorkspaceError(f"owned path escapes workspace: {relative}")
        return target


def allocated_bytes(path: Path) -> int:
    """Return allocated bytes, counting each inode at most once."""
    seen: set[tuple[int, int]] = set()
    total = 0
    stack = [path]
    while stack:
        current = stack.pop()
        status = current.lstat()
        identity = (status.st_dev, status.st_ino)
        if identity in seen:
            continue
        seen.add(identity)
        total += status.st_blocks * 512
        if current.is_dir() and not current.is_symlink():
            stack.extend(current.iterdir())
    return total
