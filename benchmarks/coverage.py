"""Independent build inputs and explicit capture boundaries.

These readers establish required direct translation units, not whole-program
coverage. Static dependencies contribute only their selected native symbols;
shared dependencies and prebuilt runtimes do not become executable IR inputs.
"""

import json
import re
import shlex
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from benchmarks.process import Measurement
from benchmarks.recipes import Target
from benchmarks.toolchains import sha256


@dataclass(frozen=True)
class SourceInput:
    path: str
    language: str
    module_hint: str | None = None


@dataclass(frozen=True)
class Exclusion:
    category: str
    items: tuple[str, ...]
    reason: str
    count: int | None = None


@dataclass(frozen=True)
class Coverage:
    required: tuple[SourceInput, ...]
    exclusions: tuple[Exclusion, ...]
    evidence: dict[str, str]
    failures: tuple[str, ...] = ()

    @property
    def complete(self) -> bool:
        return bool(self.required) and not self.failures


def _source(path: Path, language: str | None = None) -> SourceInput:
    language = language or ({".c": "C", ".rs": "Rust"}.get(path.suffix, "CXX"))
    return SourceInput(str(path.resolve()), language)


def cmake_coverage(target: Target, source: Path, build: Path) -> Coverage:
    required: dict[str, SourceInput] = {}
    exclusions: list[Exclusion] = []
    evidence: dict[str, str] = {}
    failures: list[str] = []
    try:
        replies = build / ".cmake/api/v1/reply"
        indexes = list(replies.glob("index-*.json"))
        if len(indexes) != 1:
            raise ValueError("expected one current CMake file API index")
        index = json.loads(indexes[0].read_text())
        model_path = replies / index["reply"]["codemodel-v2"]["jsonFile"]
        model = json.loads(model_path.read_text())
        configurations = model["configurations"]
        if len(configurations) != 1:
            raise ValueError("multiple CMake configurations are unsupported")
        entries = configurations[0]["targets"]
        by_id = {entry["id"]: entry for entry in entries}
        entry = next(e for e in entries if e["name"] == target.build_target)
        database = build / "compile_commands.json"
        commands = json.loads(database.read_text())
        compiled = {
            str((Path(c["directory"]) / c["file"]).resolve()) for c in commands
        }
        for path in (indexes[0], model_path, database):
            evidence[str(path)] = sha256(path)
        visited: set[str] = set()

        def visit(identifier: str, direct: bool = False) -> None:
            if identifier in visited:
                return
            visited.add(identifier)
            path = replies / by_id[identifier]["jsonFile"]
            data = json.loads(path.read_text())
            evidence[str(path)] = sha256(path)
            kind = data["type"]
            if not direct and kind in (
                "STATIC_LIBRARY",
                "SHARED_LIBRARY",
                "MODULE_LIBRARY",
            ):
                reason = (
                    "unused static archive members are not required executable inputs"
                    if kind == "STATIC_LIBRARY"
                    else "shared library bodies are not executable inputs"
                )
                exclusions.append(
                    Exclusion(kind.lower(), (data["name"],), reason, 1)
                )
                return
            groups = data.get("compileGroups", [])
            for item in data.get("sources", []):
                if "compileGroupIndex" not in item:
                    continue
                language = groups[item["compileGroupIndex"]]["language"]
                base = build if item.get("isGenerated") else source
                path = (base / item["path"]).resolve()
                if language not in ("C", "CXX"):
                    exclusions.append(
                        Exclusion(
                            "assembly-or-other-language",
                            (str(path),),
                            f"{language} has no C/C++ bitcode coverage requirement",
                            1,
                        )
                    )
                    continue
                if str(path) not in compiled:
                    failures.append(
                        f"missing compile command evidence for source {path}"
                    )
                required[str(path)] = _source(path, language)
            for dependency in data.get("dependencies", []):
                visit(dependency["id"])

        visit(entry["id"], True)
    except (
        OSError,
        ValueError,
        KeyError,
        StopIteration,
        IndexError,
        TypeError,
    ) as error:
        failures.append(f"incomplete CMake source evidence: {error}")
    exclusions.append(
        Exclusion(
            "runtime-and-external-libraries",
            (),
            "prebuilt runtime and external library bodies are outside direct source coverage",
        )
    )
    if not required:
        failures.append("no independently evidenced direct sources")
    return Coverage(
        tuple(required.values()), tuple(exclusions), evidence, tuple(failures)
    )


def _autotools_commands(line: str) -> tuple[tuple[str, ...], ...]:
    """Read observed compiler/archive command segments, never configure prose.

    Shell expressions are not evaluated. Libtool's expanded command output is
    authoritative; its shell launcher, depbase assignment and move are ignored.
    """
    line = re.sub(r"^libtool: (?:compile|link):\s*", "", line)
    lexer = shlex.shlex(line, posix=True, punctuation_chars=";&|")
    lexer.whitespace_split = True
    lexer.commenters = ""
    try:
        tokens = list(lexer)
    except ValueError:
        return ()
    segments: list[tuple[str, ...]] = []
    current: list[str] = []
    for token in [*tokens, ";"]:
        if token and set(token) <= set(";&|"):
            if current and re.fullmatch(
                r"(?:.*-)?(?:clang\+\+|clang|g\+\+|gcc|c\+\+|cc|cxx|ar)(?:-[0-9.]+)?",
                Path(current[0]).name,
            ):
                segments.append(tuple(current))
            current = []
        else:
            current.append(token)
    return tuple(segments)


def autotools_coverage(
    target: Target,
    source: Path,
    build: Path,
    records: tuple[Measurement, ...],
) -> Coverage:
    """Follow observed verbose link inputs to observed compile inputs.

    Generated Makefiles are retained and hashed. We deliberately do not guess
    source names from object stems or glob all library members into an app.
    Missing/response-file/unsupported link evidence fails this gate.
    """
    evidence = {str(p): sha256(p) for p in build.rglob("Makefile")}
    objects: dict[str, SourceInput] = {}
    links: dict[str, list[str]] = {}
    failures: list[str] = []
    exclusions: list[Exclusion] = []
    if not evidence:
        failures.append("missing generated Autotools Makefiles")
    for record in records:
        if record.returncode != 0:
            failures.append(f"failed build evidence: {record.argv}")
        cwd = Path(record.cwd)
        if "-C" in record.argv:
            cwd /= record.argv[record.argv.index("-C") + 1]
        path = Path(record.stdout)
        evidence[str(path)] = sha256(path)
        directories: list[tuple[int, Path]] = []
        initial_cwd = cwd
        # Make prints continued recipes as physical lines. Join shell escaped
        # newlines before tokenizing; this also retains Automake's depbase
        # assignment followed by its compiler command and mv dependency step.
        text = path.read_text(errors="replace").replace("\\\n", "")
        for line in text.splitlines():
            directory = re.search(
                r"make(?:\[(\d+)\])?: (Entering|Leaving) directory ['`](.*)'",
                line,
            )
            if directory:
                level = int(directory[1] or 0)
                entry = Path(directory[3])
                if directory[2] == "Entering":
                    if any(n >= level for n, _ in directories):
                        failures.append(
                            "ambiguous interleaved recursive make directories"
                        )
                    directories.append((level, entry))
                else:
                    if not directories or directories[-1] != (level, entry):
                        failures.append(
                            "unmatched recursive make leaving directory"
                        )
                    else:
                        directories.pop()
                cwd = directories[-1][1] if directories else initial_cwd
                continue
            for args in _autotools_commands(line):
                if any(a.startswith("@") for a in args):
                    failures.append(
                        "response-file build evidence is unsupported in Autotools audit"
                    )
                    continue
                if "-o" in args and args.index("-o") + 1 < len(args):
                    output = str((cwd / args[args.index("-o") + 1]).resolve())
                    inputs = [
                        a
                        for a in args
                        if Path(a).suffix
                        in (".c", ".cc", ".cpp", ".cxx", ".S", ".s")
                    ]
                    if "-c" in args and len(inputs) == 1:
                        p = cwd / inputs[0]
                        objects[output] = _source(
                            p, "ASM" if p.suffix.lower() == ".s" else None
                        )
                    elif "-c" not in args:
                        links[output] = [
                            str((cwd / a).resolve())
                            for a in args
                            if Path(a).suffix
                            in (".o", ".lo", ".a", ".la", ".so", ".dylib")
                        ]
                elif "ar" in Path(args[0]).name:
                    archives = [
                        i for i, a in enumerate(args) if a.endswith(".a")
                    ]
                    if archives:
                        i = archives[0]
                        links[str((cwd / args[i]).resolve())] = [
                            str((cwd / a).resolve())
                            for a in args[i + 1 :]
                            if a.endswith(".o")
                        ]
    required: dict[str, SourceInput] = {}
    artifact = str((build / target.artifact).resolve())
    if artifact not in links:
        failures.append(f"missing observed link inputs for {artifact}")
    for item in links.get(artifact, []):
        if item in objects:
            entry = objects[item]
            if entry.language == "ASM":
                exclusions.append(
                    Exclusion(
                        "assembly",
                        (entry.path,),
                        "assembly has no LLVM IR source requirement",
                        1,
                    )
                )
            else:
                required[entry.path] = entry
        elif Path(item).suffix in (".a", ".la", ".so", ".dylib"):
            exclusions.append(
                Exclusion(
                    "library-dependency",
                    (item,),
                    "unused static members/shared bodies are not direct inputs",
                    1,
                )
            )
        else:
            failures.append(
                f"missing observed compilation for direct object {item}"
            )
    if not evidence or not required:
        failures.append("no independently evidenced Autotools direct sources")
    exclusions.append(
        Exclusion("runtime", (), "prebuilt runtime is outside source coverage")
    )
    return Coverage(
        tuple(required.values()), tuple(exclusions), evidence, tuple(failures)
    )


def cargo_coverage(
    target: Target,
    source: Path,
    build: Path,
    records: tuple[Measurement, ...],
    *,
    project_package: str,
) -> Coverage:
    required: dict[str, SourceInput] = {}
    dependencies: set[str] = set()
    host_tools: set[str] = set()
    evidence: dict[str, str] = {}
    failures: list[str] = []
    artifact_found = False
    for record in records:
        if record.returncode != 0:
            failures.append("Cargo build evidence reports a failed command")
        for path in (Path(record.stdout), Path(record.stderr)):
            evidence[str(path)] = sha256(path)
        for line in (
            Path(record.stdout).read_text(errors="replace").splitlines()
        ):
            try:
                data: Any = json.loads(line)
            except ValueError:
                continue
            if (
                not isinstance(data, dict)
                or data.get("reason") != "compiler-artifact"
            ):
                continue
            detail = data["target"]
            if "custom-build" in detail.get("kind", ()):
                host_tools.add(detail["src_path"])
                continue
            filenames = data["filenames"]
            selected = str(build / target.artifact) in filenames
            artifact_found |= selected
            src = Path(detail["src_path"])
            if src.is_relative_to(source) and (
                detail["name"].replace("-", "_")
                == project_package.replace("-", "_")
                or selected
            ):
                required[str(src)] = SourceInput(
                    str(src), "Rust", detail["name"].replace("-", "_")
                )
            else:
                dependencies.add(data["package_id"])
    _, invocation_failures = cargo_invocation_audit(records)
    failures.extend(invocation_failures)
    if not artifact_found or not required:
        failures.append("missing Cargo project artifact/source evidence")
    exclusions = (
        Exclusion(
            "host-build-tools",
            tuple(sorted(host_tools)),
            "host build scripts configure the build and are not target crate bodies",
            len(host_tools),
        ),
        Exclusion(
            "dependency-crates",
            tuple(sorted(dependencies)),
            "dependency crate bodies are outside project-symbol/source predicate",
            len(dependencies),
        ),
        Exclusion(
            "native-dependency-and-assembly",
            (),
            "Cargo artifacts do not enumerate build-script C/C++/assembly inputs; audit separately before claiming dependency coverage",
        ),
        Exclusion(
            "prebuilt-runtime",
            (),
            "prebuilt Rust standard library/runtime has no source-module requirement",
        ),
    )
    return Coverage(
        tuple(required.values()), exclusions, evidence, tuple(failures)
    )


def cargo_invocation_audit(
    records: tuple[Measurement, ...],
) -> tuple[int, tuple[str, ...]]:
    """Check every observed host and target rustc call for matched one-CGU work."""
    calls = 0
    failures: list[str] = []
    for record in records:
        if record.returncode != 0:
            failures.append(
                "Cargo invocation evidence contains a failed build"
            )
        for line in (
            Path(record.stderr).read_text(errors="replace").splitlines()
        ):
            if "Running `" not in line or "--crate-name" not in line:
                continue
            calls += 1
            if not re.search(r"(?:-C\s+|\b)codegen-units=1(?:\s|`|$)", line):
                failures.append(
                    "Cargo rustc invocation lacks explicit codegen-units=1"
                )
    if not calls:
        failures.append("missing verbose target/host rustc evidence")
    return calls, tuple(failures)
