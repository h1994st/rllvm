"""Explicit tool and dependency identities for fixture preparation."""

import hashlib
import os
import re
import shutil
import sys
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from benchmarks.process import Command, Measurement, SpawnFailure, checked
from benchmarks.records import SCHEMA_VERSION, append_record


class ToolchainError(RuntimeError):
    """A requested tool or native dependency cannot support this profile."""


def child_environment(
    parent: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Construct a small environment; never inherit compiler flags or secrets."""
    parent = os.environ if parent is None else parent
    allowed = (
        "PATH",
        "HOME",
        "TMPDIR",
        "SDKROOT",
        "DEVELOPER_DIR",
        "CARGO_HOME",
        "RUSTUP_HOME",
    )
    env = {key: parent[key] for key in allowed if key in parent}
    env.update(
        LC_ALL="C",
        LANG="C",
        TZ="UTC",
        GIT_CONFIG_NOSYSTEM="1",
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_TERMINAL_PROMPT="0",
    )
    return env


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


@dataclass(frozen=True)
class Tool:
    # Keep invocation spelling: clang++ and rustup dispatch using argv[0].
    path: str
    realpath: str
    sha256: str
    version: str


@dataclass(frozen=True)
class Dependency:
    name: str
    prefix: str
    version: str
    libraries: dict[str, str]
    cflags: str = ""
    libs: str = ""


@dataclass(frozen=True)
class Toolchain:
    host: str
    tools: dict[str, Tool]
    environment: dict[str, str]
    dependencies: tuple[Dependency, ...] = ()
    records: tuple[Measurement, ...] = ()
    generator: str = "Ninja"
    rust_host: str | None = None
    limitations: tuple[str, ...] = ()

    def path(self, name: str) -> str:
        try:
            return self.tools[name].path
        except KeyError as error:
            raise ToolchainError(f"required tool not found: {name}") from error

    def manifest(self) -> dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_manifest(cls, value: Mapping[str, Any]) -> Toolchain:
        return cls(
            host=value["host"],
            tools={key: Tool(**item) for key, item in value["tools"].items()},
            environment=dict(value["environment"]),
            dependencies=tuple(Dependency(**x) for x in value["dependencies"]),
            records=tuple(
                measurement_from_manifest(x) for x in value["records"]
            ),
            generator=value["generator"],
            rust_host=value["rust_host"],
            limitations=tuple(value["limitations"]),
        )

    @classmethod
    def discover(
        cls,
        names: Sequence[str],
        logs: Path,
        *,
        paths: Mapping[str, Path] | None = None,
        environment: Mapping[str, str] | None = None,
        host: str | None = None,
    ) -> Toolchain:
        host = sys.platform if host is None else host
        if host not in ("darwin", "linux"):
            raise ToolchainError("workflow benchmarks support macOS and Linux")
        env = child_environment(environment)
        env["RLLVM_CONFIG"] = str(logs.absolute() / "rllvm-config.toml")
        paths = {} if paths is None else paths
        tools: dict[str, Tool] = {}
        records = []
        run_id = uuid.uuid4().hex
        for index, name in enumerate(dict.fromkeys(names)):
            candidate = (
                str(paths[name])
                if name in paths
                else shutil.which(name, path=env.get("PATH"))
            )
            if candidate is None or not os.access(candidate, os.X_OK):
                raise ToolchainError(f"required tool not found: {name}")
            path = Path(candidate).absolute()
            args = (
                ("-vV",)
                if name == "rustc"
                else ("--rllvm-version",)
                if name in ("rllvm-cc", "rllvm-cxx", "rllvm-rustc")
                else ("--version",)
            )
            result = checked(
                Command((str(path), *args), Path.cwd(), env),
                logs,
                f"{run_id}-{index}-{name}",
            )
            records.append(result)
            version = (
                Path(result.stdout).read_text()
                + Path(result.stderr).read_text()
            ).strip()
            tools[name] = Tool(
                str(path), str(path.resolve()), sha256(path), version
            )
        rust_host = None
        if "rustc" in tools:
            match = re.search(r"^host: (.+)$", tools["rustc"].version, re.M)
            rust_host = match[1] if match else None
        _compatible_llvm(tools)
        return cls(
            host, tools, env, records=tuple(records), rust_host=rust_host
        )


def _compatible_llvm(tools: Mapping[str, Tool]) -> None:
    if not any(name in tools for name in ("llvm-dis", "llvm-link", "opt")):
        return
    versions: dict[str, int] = {}
    for name in ("rustc", "clang", "clang++", "llvm-dis", "llvm-link", "opt"):
        if name not in tools:
            continue
        pattern = (
            r"LLVM version:\s*(\d+)"
            if name == "rustc"
            else r"(?:clang|LLVM) version\s+(\d+)"
        )
        match = re.search(pattern, tools[name].version)
        if not match:
            raise ToolchainError(f"cannot identify LLVM version of {name}")
        versions[name] = int(match[1])
    if len(set(versions.values())) > 1:
        raise ToolchainError(f"incompatible LLVM producer/readers: {versions}")


def resolve_dependencies(
    toolchain: Toolchain,
    required: Sequence[str],
    prefixes: Mapping[str, Path],
    logs: Path,
) -> tuple[Dependency, ...]:
    """Resolve explicitly selected native prefixes, retaining library hashes.

    libev does not consistently ship pkg-config metadata, so its header is the
    version authority. Other libraries use pkg-config restricted to the supplied
    prefixes. Configure evidence must subsequently confirm these identities.
    """
    result = []
    env = dict(toolchain.environment)
    missing = set(required) - prefixes.keys()
    if missing:
        raise ToolchainError(
            f"provide dependency prefixes for {sorted(missing)}"
        )
    env["PKG_CONFIG_LIBDIR"] = os.pathsep.join(
        str(prefixes[name] / suffix)
        for name in required
        for suffix in ("lib/pkgconfig", "lib64/pkgconfig", "share/pkgconfig")
    )
    env["PKG_CONFIG_PATH"] = ""
    run_id = uuid.uuid4().hex
    stems = {
        "libev": ("ev",),
        "openssl": ("ssl", "crypto"),
        "zlib": ("z",),
        "libcares": ("cares",),
    }
    for name in required:
        prefix = prefixes[name].absolute()
        cflags = libs = ""
        if name == "libev":
            header = prefix / "include/ev.h"
            if not header.is_file():
                raise ToolchainError(f"missing libev header: {header}")
            contents = header.read_text()
            values = [
                re.search(rf"#define\s+EV_VERSION_{part}\s+(\d+)", contents)
                for part in ("MAJOR", "MINOR")
            ]
            if not all(values):
                raise ToolchainError(
                    f"cannot identify libev version: {header}"
                )
            version = ".".join(value[1] for value in values if value)
        else:
            outputs = []
            for flag in ("--modversion", "--cflags", "--libs"):
                measured = checked(
                    Command(
                        (toolchain.path("pkg-config"), flag, name),
                        Path.cwd(),
                        env,
                    ),
                    logs,
                    f"{run_id}-{name}-{flag[2:]}",
                )
                append_record(
                    logs / "dependency-commands.jsonl",
                    {
                        "schema_version": SCHEMA_VERSION,
                        "kind": "dependency-preparation",
                        "environment": env,
                        "measurement": asdict(measured),
                    },
                )
                outputs.append(Path(measured.stdout).read_text().strip())
            version, cflags, libs = outputs
        libraries = {}
        for stem in stems[name]:
            matches = [
                file
                for folder in ("lib", "lib64")
                for file in (prefix / folder).glob(f"lib{stem}.*")
                if file.suffix in (".a", ".so", ".dylib")
                or ".so." in file.name
            ]
            if not matches:
                raise ToolchainError(f"missing {name} library under {prefix}")
            for file in matches:
                libraries[str(file.resolve())] = sha256(file)
        result.append(
            Dependency(name, str(prefix), version, libraries, cflags, libs)
        )
    return tuple(result)


def measurement_from_manifest(value: Mapping[str, Any]) -> Measurement:
    data: dict[str, Any] = dict(value)
    data["argv"] = tuple(data["argv"])
    failure = data.get("failure")
    data["failure"] = SpawnFailure(**failure) if failure is not None else None
    return Measurement(**data)
