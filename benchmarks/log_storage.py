"""Share immutable completed validation logs without reusing validation work.

Only call after every writer/reader for the selected validation has finished.
The run owns these files and must never write them again. The private pool lives
for the entire run, so every evidence pathname remains readable independently.
"""

import hashlib
import io
import os
import stat
import uuid
from pathlib import Path
from typing import Any, BinaryIO

from benchmarks.workspace import Workspace, WorkspaceError


def _owned_path(workspace: Workspace, path: Path) -> Path:
    relative = path.absolute().relative_to(workspace.root)
    if not relative.parts or ".." in relative.parts:
        raise WorkspaceError(f"not a named owned descendant: {path}")
    current = workspace.root
    for part in relative.parts:
        current /= part
        if current.is_symlink():
            raise WorkspaceError(f"symlink is not an owned log: {current}")
    return current


def _open(path: Path) -> io.BufferedReader:
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise WorkspaceError(f"not a regular log file: {path}")
    return io.BufferedReader(io.FileIO(descriptor, "rb", closefd=True))


def _same_bytes(first: BinaryIO, second: BinaryIO) -> bool:
    first.seek(0)
    second.seek(0)
    while True:
        block = first.read(1024 * 1024)
        if block != second.read(1024 * 1024):
            return False
        if not block:
            return True


def deduplicate_logs(workspace: Workspace, logs: Path) -> dict[str, Any]:
    """Best effort; preserve originals on ownership/link/storage failures.

    bytes_avoided counts allocated blocks freed, not logical log length. Exact
    byte comparisons defend against a corrupt pool entry or a hash collision.
    """
    result: dict[str, Any] = {
        "files_shared": 0,
        "bytes_avoided": 0,
        "unavailable": [],
    }
    try:
        Workspace.create(workspace.root)  # Revalidate the ownership marker.
        logs = _owned_path(workspace, logs)
        pool = _owned_path(workspace, workspace.root / ".validation-log-pool")
        pool.mkdir(mode=0o700, exist_ok=True)
        pending = list(logs.iterdir())
    except (OSError, ValueError, WorkspaceError) as error:
        result["unavailable"].append(str(error))
        return result
    while pending:
        path = pending.pop()
        temporary = None
        try:
            path = _owned_path(workspace, path)
            if path.is_dir():
                pending.extend(path.iterdir())
                continue
            with _open(path) as original:
                status = os.fstat(original.fileno())
                digest = hashlib.file_digest(original, "sha256").hexdigest()
                stored = _owned_path(workspace, pool / digest)
                if not stored.exists():
                    os.link(path, stored, follow_symlinks=False)
                    continue
                with _open(stored) as shared:
                    shared_status = os.fstat(shared.fileno())
                    if (status.st_dev, status.st_ino) == (
                        shared_status.st_dev,
                        shared_status.st_ino,
                    ):
                        continue
                    if not _same_bytes(original, shared):
                        raise WorkspaceError(
                            f"pool content mismatch: {stored}"
                        )
                temporary = path.with_name(f".dedup-{uuid.uuid4().hex}")
                os.link(stored, temporary, follow_symlinks=False)
                os.replace(temporary, path)
                result["files_shared"] += 1
                if status.st_nlink == 1:
                    result["bytes_avoided"] += status.st_blocks * 512
        except (OSError, ValueError, WorkspaceError) as error:
            result["unavailable"].append(f"{path}: {error}")
        finally:
            if temporary is not None and temporary.exists():
                temporary.unlink()
    return result
