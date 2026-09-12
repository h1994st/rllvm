"""Completed evidence logs retain paths/bytes while sharing owned storage."""

import hashlib
import os

import pytest

from benchmarks.workspace import Workspace


def test_identical_completed_logs_share_storage_and_preserve_every_path(
    tmp_path,
):
    from benchmarks.log_storage import deduplicate_logs

    workspace = Workspace.create(tmp_path / "run")
    first = workspace.reset_directory("validation-logs/first") / "ir.stdout"
    second = workspace.reset_directory("validation-logs/second") / "ir.stdout"
    different = second.with_name("different.stdout")
    content = b"module IR\n" * 1000
    first.write_bytes(content)
    second.write_bytes(content)
    different.write_bytes(b"another module\n")
    original_blocks = second.stat().st_blocks * 512
    deduplicate_logs(workspace, first.parent)
    result = deduplicate_logs(workspace, second.parent)
    assert first.samefile(second)
    assert not different.samefile(first)
    assert first.read_bytes() == second.read_bytes() == content
    assert different.read_bytes() == b"another module\n"
    assert result["bytes_avoided"] == original_blocks
    assert result["files_shared"] == 1
    assert result["unavailable"] == []
    repeated = deduplicate_logs(workspace, second.parent)
    assert repeated["bytes_avoided"] == 0


def test_external_log_and_directory_symlinks_are_never_followed(tmp_path):
    from benchmarks.log_storage import deduplicate_logs

    workspace = Workspace.create(tmp_path / "run")
    logs = workspace.reset_directory("validation-logs")
    outside = tmp_path / "outside"
    outside.mkdir()
    sentinel = outside / "sentinel"
    sentinel.write_bytes(b"private evidence")
    inode = sentinel.stat().st_ino
    (logs / "file").symlink_to(sentinel)
    (logs / "directory").symlink_to(outside, target_is_directory=True)
    result = deduplicate_logs(workspace, logs)
    assert result["files_shared"] == 0
    assert result["unavailable"]
    assert (logs / "file").is_symlink()
    assert (logs / "directory").is_symlink()
    assert sentinel.read_bytes() == b"private evidence"
    assert sentinel.stat().st_ino == inode
    assert sentinel.stat().st_nlink == 1


@pytest.mark.parametrize("pool_symlink", [False, True])
def test_unusable_pool_preserves_original_logs(tmp_path, pool_symlink):
    from benchmarks.log_storage import deduplicate_logs

    workspace = Workspace.create(tmp_path / "run")
    logs = workspace.reset_directory("validation-logs")
    log = logs / "ir.stdout"
    log.write_bytes(b"module")
    pool = workspace.root / ".validation-log-pool"
    if pool_symlink:
        outside = tmp_path / "outside"
        outside.mkdir()
        pool.symlink_to(outside, target_is_directory=True)
    else:
        pool.mkdir()
        (pool / hashlib.sha256(b"module").hexdigest()).write_bytes(b"wrong")
    result = deduplicate_logs(workspace, logs)
    assert result["unavailable"]
    assert log.read_bytes() == b"module"
    assert log.stat().st_nlink == 1
    if pool_symlink:
        assert list(outside.iterdir()) == []


def test_unsupported_hardlinks_preserve_both_originals(tmp_path, monkeypatch):
    from benchmarks.log_storage import deduplicate_logs

    workspace = Workspace.create(tmp_path / "run")
    logs = workspace.reset_directory("validation-logs")
    first, second = logs / "first", logs / "second"
    first.write_bytes(b"module")
    second.write_bytes(b"module")

    def unavailable(*args, **kwargs):
        raise OSError("hard links unavailable")

    monkeypatch.setattr(os, "link", unavailable)
    result = deduplicate_logs(workspace, logs)
    assert result["unavailable"]
    assert result["bytes_avoided"] == 0
    assert first.read_bytes() == second.read_bytes() == b"module"
    assert not first.samefile(second)
