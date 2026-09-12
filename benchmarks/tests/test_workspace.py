import json
import multiprocessing
import os
import unittest
from multiprocessing.connection import Connection
from pathlib import Path
from tempfile import TemporaryDirectory

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


class WorkspaceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def test_reset_directory_replaces_an_owned_descendant(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        build = workspace.root / "build"
        build.mkdir()
        (build / "stale").write_text("old")

        reset = workspace.reset_directory("build")

        self.assertEqual(reset, build)
        self.assertTrue(build.is_dir())
        self.assertFalse((build / "stale").exists())

    def test_reset_file_removes_an_owned_file(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        output = workspace.root / "records" / "run.json"
        output.parent.mkdir()
        output.write_text("stale")

        reset = workspace.reset_file(Path("records") / "run.json")

        self.assertEqual(reset, output)
        self.assertFalse(output.exists())
        self.assertTrue(output.parent.is_dir())

    def test_reset_rejects_unrelated_existing_directory(self) -> None:
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        sentinel = unrelated / "keep"
        sentinel.write_text("original")
        workspace = Workspace(
            unrelated,
            unrelated / ".rllvm-workflow-workspace.json",
        )

        with self.assertRaises(WorkspaceError):
            workspace.reset_directory("build")

        self.assertEqual(sentinel.read_text(), "original")

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

        with self.assertRaises(WorkspaceError):
            workspace.reset_directory("build")

        self.assertEqual(sentinel.read_text(), "original")

    def test_create_rejects_unrelated_existing_directory(self) -> None:
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        sentinel = unrelated / "keep"
        sentinel.write_text("original")

        with self.assertRaises(WorkspaceError):
            Workspace.create(unrelated)

        self.assertEqual(sentinel.read_text(), "original")

    def test_create_rejects_symlink_without_touching_target(self) -> None:
        target = self.root / "outside"
        target.mkdir()
        sentinel = target / "keep"
        sentinel.write_text("original")
        link = self.root / "workspace"
        link.symlink_to(target, target_is_directory=True)

        with self.assertRaises(WorkspaceError):
            Workspace.create(link)

        self.assertEqual(sentinel.read_text(), "original")
        self.assertTrue(link.is_symlink())

    def test_reset_directory_unlinks_leaf_symlink_only(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        outside = self.root / "outside"
        outside.mkdir()
        sentinel = outside / "keep"
        sentinel.write_text("original")
        link = workspace.root / "build"
        link.symlink_to(outside, target_is_directory=True)

        reset = workspace.reset_directory("build")

        self.assertEqual(sentinel.read_text(), "original")
        self.assertFalse(reset.is_symlink())
        self.assertTrue(reset.is_dir())

    def test_reset_rejects_parent_traversal_and_preserves_data(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        outside = self.root / "outside"
        outside.mkdir()
        sentinel = outside / "keep"
        sentinel.write_text("original")

        with self.assertRaises(WorkspaceError):
            workspace.reset_directory(Path("..") / "outside")

        self.assertEqual(sentinel.read_text(), "original")

    def test_reset_requires_a_valid_ownership_marker(self) -> None:
        workspace = Workspace.create(self.root / "workspace")
        build = workspace.root / "build"
        build.mkdir()
        sentinel = build / "keep"
        sentinel.write_text("original")
        workspace.marker.write_text("{}")

        with self.assertRaises(WorkspaceError):
            workspace.reset_directory("build")

        self.assertEqual(sentinel.read_text(), "original")

    def test_allocated_bytes_counts_hard_linked_inode_once(self) -> None:
        tree = self.root / "tree"
        tree.mkdir()
        data = tree / "data"
        data.write_bytes(b"x" * 8192)
        os.link(data, tree / "hard-link")
        expected = tree.stat().st_blocks * 512 + data.stat().st_blocks * 512

        self.assertEqual(allocated_bytes(tree), expected)

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
            self.assertEqual(parent_connection.recv(), "locked")
            holder_state = lock_path.read_bytes()

            with self.assertRaises(RunLockError):
                with RunLock(lock_path):
                    pass

            self.assertEqual(lock_path.read_bytes(), holder_state)
        finally:
            parent_connection.send("release")
            process.join(5)
            if process.is_alive():
                process.kill()
                process.join()
        self.assertEqual(process.exitcode, 0)

    def test_lock_file_inode_is_stable_across_releases(self) -> None:
        lock_path = self.root / "run.lock"

        with RunLock(lock_path):
            first_inode = lock_path.stat().st_ino
        with RunLock(lock_path):
            second_inode = lock_path.stat().st_ino

        self.assertEqual(second_inode, first_inode)


if __name__ == "__main__":
    unittest.main()
