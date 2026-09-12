"""Snapshot boundaries use real repositories, including dirty submodules."""

import os
import subprocess
from dataclasses import replace
from pathlib import Path

import pytest

from benchmarks.fixtures import FixtureError, prepare_fixture
from benchmarks.recipes import get_recipe
from benchmarks.records import read_json
from benchmarks.toolchains import Toolchain, child_environment
from benchmarks.workspace import Workspace


def git(repository: Path, *args: str) -> str:
    return subprocess.check_output(
        ("git", "-c", "protocol.file.allow=always", *args),
        cwd=repository,
        env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"},
        stderr=subprocess.PIPE,
        text=True,
    ).strip()


def repository_at(path: Path) -> str:
    path.mkdir()
    git(path, "init", "-q", "-b", "main")
    git(path, "config", "user.name", "Fixture")
    git(path, "config", "user.email", "fixture@example.invalid")
    (path / "value").write_text("first\n")
    git(path, "add", ".")
    git(path, "commit", "-qm", "first")
    return git(path, "rev-parse", "HEAD")


class TestFixtures:
    @pytest.fixture(autouse=True)
    def _fixture_context(self, tmp_path: Path) -> None:
        self.root = tmp_path / "fixture space"
        self.root.mkdir()
        self.repository = self.root / "input source"
        self.commit = repository_at(self.repository)
        self.workspace = Workspace.create(self.root / "owned")
        self.recipe = replace(
            get_recipe("nghttp2-c-cmake"),
            repository_url=str(self.repository),
            commit=self.commit,
            required_submodules=(),
        )
        self.tools = Toolchain.discover(
            ("git",),
            self.root / "tool logs",
            environment=child_environment(os.environ),
        )

    def test_old_revision_ignores_dirty_input_and_preserves_it(self) -> None:
        (self.repository / "value").write_text("second\n")
        git(self.repository, "commit", "-qam", "second")
        head = git(self.repository, "rev-parse", "HEAD")
        (self.repository / "value").write_text("uncommitted\n")
        before = git(self.repository, "status", "--porcelain=v1")
        prepared = prepare_fixture(
            self.recipe, self.repository, self.workspace.root, self.tools
        )
        assert (prepared.source / "value").read_text() == "first\n"
        assert (self.repository / "value").read_text() == "uncommitted\n"
        assert git(self.repository, "rev-parse", "HEAD") == head
        assert git(self.repository, "status", "--porcelain=v1") == before
        assert prepared.commit == self.commit
        assert prepared.preparation_records
        assert (
            type(prepared).from_manifest(read_json(prepared.manifest_path))
            == prepared
        )

    def test_missing_required_gitlink_fails_without_profile_reduction(self):
        recipe = replace(self.recipe, required_submodules=("missing",))
        with pytest.raises(FixtureError, match="required gitlink"):
            prepare_fixture(
                recipe, self.repository, self.workspace.root, self.tools
            )

    def test_submodule_uses_gitlink_instead_of_dirty_revised_checkout(self):
        dependency = self.root / "dependency"
        pin = repository_at(dependency)
        git(self.repository, "submodule", "add", str(dependency), "dependency")
        git(self.repository, "commit", "-qam", "pin dependency")
        recipe = replace(
            self.recipe,
            commit=git(self.repository, "rev-parse", "HEAD"),
            required_submodules=("dependency",),
        )
        (dependency / "value").write_text("new upstream\n")
        git(dependency, "commit", "-qam", "new upstream")
        checkout = self.repository / "dependency"
        git(checkout, "fetch")
        git(checkout, "checkout", "--detach", "origin/main")
        (checkout / "value").write_text("dirty dependency\n")
        prepared = prepare_fixture(
            recipe, self.repository, self.workspace.root, self.tools
        )
        assert (prepared.source / "dependency/value").read_text() == "first\n"
        assert prepared.submodules["dependency"] == pin
        assert (checkout / "value").read_text() == "dirty dependency\n"

    def test_revision_override_is_a_distinct_snapshot_identity(self):
        (self.repository / "value").write_text("second\n")
        git(self.repository, "commit", "-qam", "second")
        head = git(self.repository, "rev-parse", "HEAD")
        original = prepare_fixture(
            self.recipe, self.repository, self.workspace.root, self.tools
        )
        alternate = prepare_fixture(
            self.recipe,
            self.repository,
            self.workspace.root,
            self.tools,
            revision=head,
        )
        assert original.identity != alternate.identity
        assert original.source != alternate.source
        assert (alternate.source / "value").read_text() == "second\n"

    @pytest.mark.full
    def test_lock_is_copied_without_resolution(self):
        # A dependency-free package lets the actual locked fetch run offline.
        (self.repository / "Cargo.toml").write_text(
            '[package]\nname="quiche"\nversion="0.1.0"\nedition="2024"\n'
        )
        (self.repository / "src").mkdir()
        (self.repository / "src/lib.rs").write_text("pub fn value() {}\n")
        git(self.repository, "add", ".")
        git(self.repository, "commit", "-qm", "cargo package")
        lock = self.root / "fixed.lock"
        lock.write_text(
            'version = 4\n[[package]]\nname="quiche"\nversion="0.1.0"\n'
        )
        recipe = replace(
            get_recipe("quiche-cargo"),
            repository_url=str(self.repository),
            commit=git(self.repository, "rev-parse", "HEAD"),
            lockfile=lock,
        )
        tools = Toolchain.discover(
            ("git", "cargo", "rustc"),
            self.root / "cargo tool logs",
            environment=child_environment(os.environ),
        )
        prepared = prepare_fixture(
            recipe, self.repository, self.workspace.root, tools
        )
        assert (
            prepared.source / "Cargo.lock"
        ).read_bytes() == lock.read_bytes()
        assert prepared.lock_sha256 is not None
        assert not (self.repository / "Cargo.lock").exists()
        assert any("--locked" in r.argv for r in prepared.preparation_records)

    def test_only_declared_submodules_are_prepared(self):
        dependency = self.root / "dependency"
        repository_at(dependency)
        # This submodule's optional tests are not part of the selected profile.
        (dependency / ".gitmodules").write_text(
            '[submodule "unused-tests"]\npath = unused-tests\n'
            "url = /nonexistent-rllvm-optional-test-repository\n"
        )
        git(dependency, "add", ".gitmodules")
        git(
            dependency,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{self.commit},unused-tests",
        )
        git(dependency, "commit", "-qm", "optional test dependency")
        git(self.repository, "submodule", "add", str(dependency), "dependency")
        git(self.repository, "commit", "-qam", "required dependency")
        recipe = replace(
            self.recipe,
            commit=git(self.repository, "rev-parse", "HEAD"),
            required_submodules=("dependency",),
        )
        prepared = prepare_fixture(
            recipe, self.repository, self.workspace.root, self.tools
        )
        assert (prepared.source / "dependency/value").read_text() == "first\n"
        assert not (prepared.source / "dependency/unused-tests/.git").exists()
