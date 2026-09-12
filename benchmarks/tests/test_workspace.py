import json
import multiprocessing
import os
from multiprocessing.connection import Connection
from pathlib import Path

import pytest

from benchmarks.workspace import (
    RunLock,
    RunLockError,
    Workspace,
    WorkspaceError,
    allocated_bytes,
)


def hold_lock(path: str, connection: Connection) -> None:
    with RunLock(Path(path)):
        connection.send("locked")
        connection.recv()


class TestWorkspace:
    @pytest.fixture(autouse=True)
    def _workspace_root(self, tmp_path: Path) -> None:
        self.root = tmp_path

    def test_reset_directory_replaces_an_owned_descendant(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        build = workspace.root / "build"
        build.mkdir()
        (build / "stale").write_text("old")

        reset = workspace.reset_directory("build")

        assert reset == build
        assert build.is_dir()
        assert not (build / "stale").exists()

    def test_reset_file_removes_an_owned_file(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        output = workspace.root / "records" / "run.json"
        output.parent.mkdir()
        output.write_text("stale")

        reset = workspace.reset_file(Path("records") / "run.json")

        assert reset == output
        assert not output.exists()
        assert output.parent.is_dir()

    def test_reset_rejects_unrelated_existing_directory(self) -> None:
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        sentinel = unrelated / "keep"
        sentinel.write_text("original")
        workspace = Workspace(
            unrelated,
            unrelated / ".rllvm-workflow-workspace.json",
        )

        with pytest.raises(WorkspaceError):
            workspace.reset_directory("build")

        assert sentinel.read_text() == "original"

    def test_reset_rejects_marker_outside_workspace_root(self) -> None:
        unrelated = self.root / "unrelated"
        build = unrelated / "build"
        build.mkdir(parents=True)
        sentinel = build / "keep"
        sentinel.write_text("original")
        external_marker = self.root / "external-marker.json"
        external_marker.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "kind": "rllvm-workflow-benchmark",
                    "root": str(unrelated.resolve()),
                }
            )
        )
        workspace = Workspace(unrelated, external_marker)

        with pytest.raises(WorkspaceError):
            workspace.reset_directory("build")

        assert sentinel.read_text() == "original"

    def test_create_rejects_unrelated_existing_directory(self) -> None:
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        sentinel = unrelated / "keep"
        sentinel.write_text("original")

        with pytest.raises(WorkspaceError):
            Workspace.create(unrelated)

        assert sentinel.read_text() == "original"

    def test_create_rejects_symlink_without_touching_target(self) -> None:
        target = self.root / "outside"
        target.mkdir()
        sentinel = target / "keep"
        sentinel.write_text("original")
        link = self.root / "workspace"
        link.symlink_to(target, target_is_directory=True)

        with pytest.raises(WorkspaceError):
            Workspace.create(link)

        assert sentinel.read_text() == "original"
        assert link.is_symlink()

    def test_reset_directory_unlinks_leaf_symlink_only(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        outside = self.root / "outside"
        outside.mkdir()
        sentinel = outside / "keep"
        sentinel.write_text("original")
        link = workspace.root / "build"
        link.symlink_to(outside, target_is_directory=True)

        reset = workspace.reset_directory("build")

        assert sentinel.read_text() == "original"
        assert not reset.is_symlink()
        assert reset.is_dir()

    def test_reset_rejects_parent_traversal_and_preserves_data(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        outside = self.root / "outside"
        outside.mkdir()
        sentinel = outside / "keep"
        sentinel.write_text("original")

        with pytest.raises(WorkspaceError):
            workspace.reset_directory(Path("..") / "outside")

        assert sentinel.read_text() == "original"

    def test_reset_requires_a_valid_ownership_marker(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        build = workspace.root / "build"
        build.mkdir()
        sentinel = build / "keep"
        sentinel.write_text("original")
        workspace.marker.write_text("{}")

        with pytest.raises(WorkspaceError):
            workspace.reset_directory("build")

        assert sentinel.read_text() == "original"

    def test_allocated_bytes_counts_hard_linked_inode_once(self) -> None:
        tree = self.root / "tree"
        tree.mkdir()
        data = tree / "data"
        data.write_bytes(b"x" * 8192)
        os.link(data, tree / "hard-link")
        expected = tree.stat().st_blocks * 512 + data.stat().st_blocks * 512

        assert allocated_bytes(tree) == expected

    def test_contended_lock_does_not_truncate_holder_state(self) -> None:
        lock_path = self.root / "run.lock"
        context = multiprocessing.get_context("spawn")
        parent_connection, child_connection = context.Pipe()
        process = context.Process(
            target=hold_lock,
            args=(str(lock_path), child_connection),
        )
        process.start()
        try:
            assert parent_connection.recv() == "locked"
            holder_state = lock_path.read_bytes()

            with pytest.raises(RunLockError):
                with RunLock(lock_path):
                    pass

            assert lock_path.read_bytes() == holder_state
        finally:
            parent_connection.send("release")
            process.join(5)
            if process.is_alive():
                process.kill()
                process.join()
        assert process.exitcode == 0

    def test_lock_file_inode_is_stable_across_releases(self) -> None:
        lock_path = self.root / "run.lock"

        with RunLock(lock_path):
            first_inode = lock_path.stat().st_ino
        with RunLock(lock_path):
            second_inode = lock_path.stat().st_ino

        assert second_inode == first_inode
