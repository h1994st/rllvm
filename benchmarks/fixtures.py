"""Prepare immutable-input, private pinned snapshots before measurements."""

import hashlib
import json
import re
import shutil
import uuid
from collections.abc import Mapping
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from benchmarks.process import Command, CommandFailed, Measurement, checked
from benchmarks.recipes import Recipe
from benchmarks.records import SCHEMA_VERSION, append_record, write_json
from benchmarks.toolchains import Toolchain, measurement_from_manifest, sha256
from benchmarks.workspace import Workspace


class FixtureError(RuntimeError):
    """A private source snapshot cannot reproduce the requested fixture."""


@dataclass(frozen=True)
class PreparedFixture:
    identity: str
    source: Path
    recipe: Recipe
    commit: str
    tree: str
    submodules: dict[str, str]
    lock_sha256: str | None
    toolchain: Toolchain
    preparation_records: tuple[Measurement, ...]
    manifest_path: Path

    def manifest(self) -> dict[str, Any]:
        return {
            "schema_version": SCHEMA_VERSION,
            "kind": "prepared-fixture",
            "identity": self.identity,
            "source": str(self.source),
            "recipe": self.recipe.manifest(),
            "commit": self.commit,
            "tree": self.tree,
            "submodules": self.submodules,
            "lock_sha256": self.lock_sha256,
            "toolchain": self.toolchain.manifest(),
            "preparation_records": [
                asdict(r) for r in self.preparation_records
            ],
            "manifest_path": str(self.manifest_path),
            "targets": [
                asdict(t) for t in self.recipe.targets(self.toolchain.host)
            ],
            "limitations": [
                "Cargo target and host release work uses one codegen unit",
                "Prebuilt Rust standard libraries and assembly may be uncaptured",
            ]
            if self.recipe.build_system == "cargo"
            else [],
        }

    @classmethod
    def from_manifest(cls, value: Mapping[str, Any]) -> PreparedFixture:
        if (
            value.get("schema_version") != SCHEMA_VERSION
            or value.get("kind") != "prepared-fixture"
        ):
            raise FixtureError("unsupported prepared fixture manifest")
        return cls(
            value["identity"],
            Path(value["source"]),
            Recipe.from_manifest(value["recipe"]),
            value["commit"],
            value["tree"],
            dict(value["submodules"]),
            value["lock_sha256"],
            Toolchain.from_manifest(value["toolchain"]),
            tuple(
                measurement_from_manifest(r)
                for r in value["preparation_records"]
            ),
            Path(value["manifest_path"]),
        )


class _Preparation:
    def __init__(self, directory: Path, tools: Toolchain) -> None:
        self.directory = directory
        self.tools = tools
        self.records: list[Measurement] = []
        self.run_id = uuid.uuid4().hex

    def run(
        self,
        argv: tuple[str, ...],
        cwd: Path,
        *,
        env: dict[str, str] | None = None,
    ) -> str:
        command = Command(
            argv, cwd, dict(self.tools.environment) if env is None else env
        )
        label = f"{self.run_id}-{len(self.records):04d}"
        try:
            result = checked(command, self.directory / "logs", label)
        except CommandFailed as error:
            self._record(command, error.result)
            raise
        self._record(command, result)
        return Path(result.stdout).read_text().strip()

    def _record(self, command: Command, result: Measurement) -> None:
        self.records.append(result)
        append_record(
            self.directory / "preparation.jsonl",
            {
                "schema_version": SCHEMA_VERSION,
                "kind": "fixture-preparation",
                "environment": command.env,
                "measurement": asdict(result),
            },
        )

    def git(self, cwd: Path, *args: str) -> str:
        return self.run((self.tools.path("git"), *args), cwd)


def prepare_fixture(
    recipe: Recipe,
    local_repository: Path | None,
    owned_root: Path,
    toolchain: Toolchain,
    *,
    revision: str | None = None,
) -> PreparedFixture:
    """Clone from local objects without changing the cache checkout.

    Fetches (including Cargo fetch) occur only here. Each preparation requires a
    fresh profile directory; reruns never erase existing sources or evidence.
    Alternative revisions require a full commit id and get a distinct path.
    """
    commit = recipe.commit if revision is None else revision
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise FixtureError(
            "fixture revision must be a full 40-digit commit id"
        )
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", recipe.profile_id):
        raise FixtureError("profile id must be a safe path component")
    workspace = Workspace.create(owned_root)
    fixtures = workspace.root / "fixtures"
    if fixtures.is_symlink():
        raise FixtureError("fixture parent must not be a symlink")
    fixtures.mkdir(exist_ok=True)
    name = recipe.profile_id + (f"-{commit}" if revision is not None else "")
    directory = fixtures / name
    directory.mkdir()  # Refuse to replace any previous preparation.
    prep = _Preparation(directory, toolchain)
    source = directory / "source"
    _clone(prep, local_repository, recipe.repository_url, source, commit)
    tree = prep.git(source, "rev-parse", "HEAD^{tree}")
    submodules: dict[str, str] = {}
    for relative in recipe.required_submodules:
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts or path == Path("."):
            raise FixtureError(f"unsafe required gitlink path: {relative}")
        entry = prep.git(source, "ls-tree", "HEAD", "--", relative)
        match = re.fullmatch(r"160000 commit ([0-9a-f]{40})\t(.+)", entry)
        if not match or match[2] != relative:
            raise FixtureError(f"missing required gitlink: {relative}")
        pin = match[1]
        module_config = source / ".gitmodules"
        sections = prep.git(
            source,
            "config",
            "-f",
            str(module_config),
            "--get-regexp",
            r"^submodule\..*\.path$",
        )
        section = next(
            (
                line.rsplit(" ", 1)[0][:-5]
                for line in sections.splitlines()
                if line.endswith(" " + relative)
            ),
            None,
        )
        if section is None:
            raise FixtureError(f"no URL for required gitlink: {relative}")
        url = prep.git(
            source,
            "config",
            "-f",
            str(module_config),
            "--get",
            f"{section}.url",
        )
        if url.startswith(("./", "../")):
            # Git resolves relative submodule URLs against its remote, not cwd.
            prep.git(source, "submodule", "init", "--", relative)
            url = prep.git(source, "config", "--get", f"{section}.url")
        local = local_repository / relative if local_repository else None
        if local is not None and not (local / ".git").exists():
            local = None
        target = source / relative
        if target.exists():
            target.rmdir()  # Pinned gitlink directories must be empty.
        target.parent.mkdir(parents=True, exist_ok=True)
        _clone(prep, local, url, target, pin)
        submodules[relative] = pin
    lock_hash = None
    if recipe.build_system == "autotools":
        prep.run((toolchain.path("autoreconf"), "-fi"), source)
    if recipe.build_system == "cargo":
        if recipe.lockfile is None or not recipe.lockfile.is_file():
            raise FixtureError("benchmark-owned Cargo.lock is required")
        shutil.copyfile(recipe.lockfile, source / "Cargo.lock")
        lock_hash = sha256(source / "Cargo.lock")
        env = dict(toolchain.environment)
        env["CARGO_INCREMENTAL"] = "0"
        argv = (toolchain.path("cargo"), "fetch", "--locked")
        if toolchain.rust_host:
            argv += ("--target", toolchain.rust_host)
        prep.run(argv, source, env=env)
        if sha256(source / "Cargo.lock") != lock_hash:
            raise FixtureError("locked Cargo preparation changed Cargo.lock")
    identity = hashlib.sha256(
        json.dumps(
            {
                "recipe": recipe.manifest(),
                "commit": commit,
                "tree": tree,
                "submodules": submodules,
                "lock_sha256": lock_hash,
            },
            sort_keys=True,
        ).encode()
    ).hexdigest()
    fixture = PreparedFixture(
        identity,
        source,
        recipe,
        commit,
        tree,
        submodules,
        lock_hash,
        toolchain,
        tuple(prep.records),
        directory / "resolved.json",
    )
    write_json(fixture.manifest_path, fixture.manifest())
    return fixture


def _clone(
    prep: _Preparation,
    local: Path | None,
    url: str,
    destination: Path,
    commit: str,
) -> None:
    origin = str(local.absolute()) if local is not None else url
    prep.git(
        destination.parent,
        "clone",
        "--no-checkout",
        "--no-hardlinks",
        "--",
        origin,
        str(destination),
    )
    prep.git(destination, "remote", "set-url", "origin", url)
    try:
        prep.git(destination, "cat-file", "-e", f"{commit}^{{commit}}")
    except CommandFailed:
        prep.git(destination, "fetch", "--no-tags", "origin", commit)
    prep.git(destination, "checkout", "--detach", commit)
    actual = prep.git(destination, "rev-parse", "HEAD")
    if actual != commit:
        raise FixtureError(f"checkout differs from requested commit: {actual}")
