"""Untimed output validity gates with preserved independent evidence."""

import hashlib
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from benchmarks.coverage import Coverage, cargo_invocation_audit
from benchmarks.process import Command, Measurement, execute
from benchmarks.recipes import Recipe, Target
from benchmarks.toolchains import Toolchain, sha256


@dataclass(frozen=True)
class Check:
    failures: tuple[str, ...] = ()
    evidence: tuple[str, ...] = ()

    @property
    def valid(self) -> bool:
        return not self.failures


@dataclass(frozen=True)
class Extraction:
    output: Path
    manifest: Path
    measurement: Measurement | None = None


@dataclass(frozen=True)
class Module:
    path: str
    sha256: str
    source_filename: str
    source_paths: tuple[str, ...]


@dataclass(frozen=True)
class Validation:
    target_id: str
    failures: tuple[str, ...]
    modules: tuple[Module, ...]
    native_definitions: tuple[str, ...]
    wrapped_definitions: tuple[str, ...]
    ir_definitions: tuple[str, ...]
    coverage: Coverage | None
    records: tuple[Measurement, ...]
    evidence: dict[str, str]
    excluded_definitions: dict[str, tuple[str, ...]]
    ir_sha256: str | None
    schema_version: int = 1

    @property
    def valid(self) -> bool:
        return not self.failures

    @property
    def module_count(self) -> int:
        return len({m.path for m in self.modules})


@dataclass(frozen=True)
class ExtractionSet:
    failures: tuple[str, ...]
    per_target_module_count: dict[str, int]
    union_module_count: int
    shared_module_count: int
    schema_version: int = 1

    @property
    def valid(self) -> bool:
        return not self.failures


def _llvm_string(value: str) -> str:
    raw = re.sub(r"\\([0-9a-fA-F]{2})", lambda m: chr(int(m[1], 16)), value)
    return raw.encode("latin1").decode("utf-8", errors="surrogateescape")


def _sources(ir: str) -> tuple[str, tuple[str, ...]]:
    name = re.search(r'^source_filename = "(.*)"$', ir, re.M)
    filename = _llvm_string(name[1]) if name else ""
    paths = set()
    for match in re.finditer(
        r'!DIFile\(filename: "((?:[^"\\]|\\[0-9A-Fa-f]{2})*)", directory: "((?:[^"\\]|\\[0-9A-Fa-f]{2})*)"',
        ir,
    ):
        paths.add(str(Path(_llvm_string(match[2])) / _llvm_string(match[1])))
    return filename, tuple(sorted(paths))


def _defined_symbols(output: str) -> set[str]:
    # POSIX format: name type value [size]. Archive headers end in a colon.
    return {
        parts[0]
        for line in output.splitlines()
        if len(parts := line.split()) >= 3
        and len(parts[1]) == 1
        and parts[1] not in ("U", "?")
    }


def _project(symbol: str, target: Target, host: str) -> bool:
    spelling = (
        symbol[1:] if host == "darwin" and symbol.startswith("_") else symbol
    )
    return (
        spelling.startswith(target.symbol_prefixes)
        or any(part in spelling for part in target.symbol_contains)
        or spelling in target.definitions
    )


def validate_target(
    target: Target,
    native: Path,
    wrapped: Path,
    extracted: Extraction,
    toolchain: Toolchain,
    *,
    logs: Path,
    env: dict[str, str],
    coverage: Coverage | None = None,
) -> Validation:
    """Validate one extraction, including every recorded original module.

    Artifact paths are explicit (not build directories). Supply independent
    Coverage: absence is an incomplete sample, never an implicit pass. All
    subprocesses and their output remain in logs. No symbol demangling occurs.
    """
    failures: list[str] = []
    records: list[Measurement] = []
    evidence: dict[str, str] = {}
    modules: list[Module] = []
    excluded: dict[str, tuple[str, ...]] = {}
    counter = 0

    def invoke(tool: str, *args: str) -> str | None:
        nonlocal counter
        counter += 1
        result = execute(
            Command((toolchain.path(tool), *args), wrapped.parent, dict(env)),
            logs,
            f"{counter:05d}-{tool}",
        )
        records.append(result)
        if result.returncode != 0:
            failures.append(f"{tool} failed for {args}: {result.returncode}")
            return None
        return Path(result.stdout).read_text(errors="surrogateescape")

    for label, path in (
        ("native", native),
        ("wrapped", wrapped),
        ("extracted", extracted.output),
        ("manifest", extracted.manifest),
    ):
        if not path.is_file():
            failures.append(f"missing {label} file: {path}")
        else:
            evidence[str(path)] = sha256(path)
    if extracted.measurement is not None:
        records.append(extracted.measurement)
        if extracted.measurement.returncode != 0:
            failures.append("extraction command failed")
    paths: list[str] = []
    if extracted.manifest.is_file():
        paths = extracted.manifest.read_text(
            errors="surrogateescape"
        ).splitlines()
        if not paths or any(not p for p in paths):
            failures.append("missing or empty module entries in manifest")
    for entry in dict.fromkeys(paths):
        path = Path(entry)
        if not path.is_absolute():
            failures.append(f"unresolved relative manifest path: {path}")
            continue
        if not path.is_file():
            failures.append(f"missing recorded module: {path}")
            continue
        output = invoke("llvm-dis", str(path), "-o", "-")
        invoke("opt", "-passes=verify", "-disable-output", str(path))
        if output is not None:
            filename, sources = _sources(output)
            modules.append(
                Module(str(path.resolve()), sha256(path), filename, sources)
            )
    definitions: list[tuple[str, ...]] = []
    for label, path in (
        ("native", native),
        ("wrapped", wrapped),
        ("IR", extracted.output),
    ):
        symbols: set[str] = set()
        if path.is_file():
            output = invoke(
                "llvm-nm",
                "--defined-only",
                "--extern-only",
                "--format=posix",
                str(path),
            )
            if output is not None:
                all_symbols = _defined_symbols(output)
                symbols = {
                    s
                    for s in all_symbols
                    if _project(s, target, toolchain.host)
                }
                excluded[label] = tuple(sorted(all_symbols - symbols))
        definitions.append(tuple(sorted(symbols)))
    native_symbols, wrapped_symbols, ir_symbols = map(set, definitions)
    if native_symbols != wrapped_symbols:
        failures.append(
            f"native/wrapped project definitions differ; missing wrapped={sorted(native_symbols - wrapped_symbols)}, extra wrapped={sorted(wrapped_symbols - native_symbols)}"
        )
    missing = wrapped_symbols - ir_symbols
    if missing:
        failures.append(
            f"missing project definitions in extracted IR: {sorted(missing)}"
        )
    for name in target.definitions:
        decorated = "_" + name if toolchain.host == "darwin" else name
        for label, symbols in (
            ("native", native_symbols),
            ("wrapped", wrapped_symbols),
            ("IR", ir_symbols),
        ):
            if decorated not in symbols:
                failures.append(f"missing required {label} definition: {name}")
    if not native_symbols:
        failures.append(
            "no native project definitions matched the declared predicate"
        )
    extracted_sources: tuple[str, ...] = ()
    extracted_filename = ""
    ir_sha256 = None
    if extracted.output.is_file():
        ir = invoke("llvm-dis", str(extracted.output), "-o", "-")
        if ir is not None:
            extracted_filename, extracted_sources = _sources(ir)
            canonical = re.sub(r"^; ModuleID = .*\n", "", ir)
            ir_sha256 = hashlib.sha256(
                canonical.encode(errors="surrogateescape")
            ).hexdigest()
        invoke(
            "opt", "-passes=verify", "-disable-output", str(extracted.output)
        )
    if coverage is None:
        failures.append("missing independent source coverage evidence")
    else:
        failures.extend(coverage.failures)
        if not coverage.complete:
            failures.append("incomplete independent source coverage")
        for item in coverage.required:
            if item.module_hint:
                present = any(
                    re.match(
                        re.escape(item.module_hint) + r"[.-]",
                        m.source_filename,
                    )
                    for m in modules
                )
            else:
                present = any(
                    str(Path(m.source_filename).resolve()) == item.path
                    or item.path in m.source_paths
                    for m in modules
                )
            if not present:
                failures.append(f"missing source module: {item.path}")
            if (
                item.language in ("C", "CXX")
                and item.path not in extracted_sources
                and item.path != extracted_filename
            ):
                failures.append(
                    f"missing extracted source metadata: {item.path}; debug source evidence is required"
                )
    return Validation(
        target.id,
        tuple(failures),
        tuple(modules),
        definitions[0],
        definitions[1],
        definitions[2],
        coverage,
        tuple(records),
        evidence,
        excluded,
        ir_sha256,
    )


def validate_extraction_set(
    targets: tuple[Target, ...],
    results: dict[str, Validation],
    *,
    repeated: dict[str, Validation] | None = None,
) -> ExtractionSet:
    failures: list[str] = []
    counts: dict[str, int] = {}
    memberships: dict[str, int] = {}
    declared = {t.id for t in targets}
    if set(results) != declared:
        failures.append(
            f"declared extraction targets differ: missing={sorted(declared - results.keys())}, unexpected={sorted(results.keys() - declared)}"
        )
    if repeated is not None and set(repeated) != declared:
        failures.append(
            "repeated extraction is missing declared targets or contains unexpected targets"
        )
    for key, result in results.items():
        if not result.valid or result.target_id != key:
            failures.append(f"invalid extraction: {key}")
        counts[key] = result.module_count
        for module in {m.path for m in result.modules}:
            memberships[module] = memberships.get(module, 0) + 1
        if repeated is not None and key in repeated:
            other = repeated[key]
            identity = {(m.path, m.sha256) for m in result.modules}
            other_identity = {(m.path, m.sha256) for m in other.modules}
            if (
                not other.valid
                or other.target_id != key
                or identity != other_identity
                or result.ir_definitions != other.ir_definitions
                or result.ir_sha256 != other.ir_sha256
            ):
                failures.append(f"repeated extraction contents changed: {key}")
    return ExtractionSet(
        tuple(failures),
        counts,
        len(memberships),
        sum(n > 1 for n in memberships.values()),
    )


def validate_behavior(
    native: tuple[Measurement, ...],
    wrapped: tuple[Measurement, ...],
    *,
    expected_suffix: str | None = None,
    kind: Literal["exact", "munit"] = "exact",
) -> Check:
    failures: list[str] = []
    evidence: list[str] = []
    if not native or len(native) != len(wrapped):
        failures.append("missing corresponding behavior observations")
    for left, right in zip(native, wrapped, strict=False):
        for record in (left, right):
            if record.returncode != 0:
                failures.append(f"behavior command failed: {record.argv}")
            evidence.extend((record.stdout, record.stderr))
        a, b = Path(left.stdout).read_bytes(), Path(right.stdout).read_bytes()
        if kind == "munit":

            def outcomes(output: bytes) -> tuple:
                text = output.decode(errors="replace")
                tests = re.findall(
                    r"^(/[^\n]*?)\s*\[\s*(OK|FAIL|ERROR|SKIP)\s*\]", text, re.M
                )
                summary = re.search(
                    r"(\d+) of (\d+) \(100%\) tests successful, 0 \(0%\) test skipped\.",
                    text,
                )
                if (
                    not tests
                    or summary is None
                    or summary[1] != summary[2]
                    or int(summary[1]) != len(tests)
                    or any(status != "OK" for _, status in tests)
                ):
                    failures.append("missing or failed munit test outcomes")
                return tuple(tests)

            equal = outcomes(a) == outcomes(b)
        else:
            equal = a.replace(
                left.argv[0].encode(), b"<PROGRAM>"
            ) == b.replace(right.argv[0].encode(), b"<PROGRAM>")
        if not equal:
            failures.append("native/wrapped behavior stdout differs")
        if expected_suffix is not None and (
            expected_suffix.encode() not in a
            or expected_suffix.encode() not in b
        ):
            failures.append(
                "incremental API probe is missing the expected version suffix"
            )
    return Check(tuple(failures), tuple(evidence))


def validate_autotools_configuration(build: Path) -> Check:
    path = build / "libtool"
    try:
        contents = path.read_text()
    except OSError as error:
        return Check((f"missing libtool configuration: {error}",))
    matches = re.findall(
        r'^max_cmd_len=["\']?([^"\'\n]*)["\']?$', contents, re.M
    )
    if len(matches) != 1 or not matches[0].isdigit() or int(matches[0]) <= 0:
        return Check(
            ("invalid Autotools max_cmd_len: expected a positive integer",),
            (str(path),),
        )
    return Check(
        evidence=(str(path), hashlib.sha256(contents.encode()).hexdigest())
    )


def validate_configuration(
    recipe: Recipe,
    native_commands: tuple[Command, ...],
    wrapped_commands: tuple[Command, ...],
    tools: Toolchain,
    native_build: Path,
    wrapped_build: Path,
    *,
    native_records: tuple[Measurement, ...] = (),
    wrapped_records: tuple[Measurement, ...] = (),
) -> Check:
    """Compare full recorded workload commands after only declared mode changes.

    Normalize owned build/config paths and approved wrapper substitutions. All
    remaining argv/environment differences are failures. Cache/storage settings
    are capture mechanics, while compiler identities and options must agree.
    Feed configure and build commands, in the same order, from both modes.
    """
    import tomllib

    failures: list[str] = []
    evidence: list[str] = []
    if not native_commands or len(native_commands) != len(wrapped_commands):
        failures.append("missing matched configuration/build commands")
    mapping = {
        tools.path(wrapper): tools.path(real)
        for wrapper, real in (("rllvm-cc", "clang"), ("rllvm-cxx", "clang++"))
        if wrapper in tools.tools and real in tools.tools
    }

    def normalize(value: str, build: Path) -> str:
        value = value.replace(str(build), "<BUILD>")
        for wrapper, real in mapping.items():
            value = value.replace(wrapper, real)
        return value

    def configuration(command: Command) -> dict:
        filename = command.env.get("RLLVM_CONFIG")
        if not filename:
            failures.append("configuration command lacks scratch RLLVM_CONFIG")
            return {}
        path = Path(filename)
        evidence.append(str(path))
        try:
            data = tomllib.loads(path.read_text())
        except (OSError, ValueError) as error:
            failures.append(
                f"missing/unreadable scratch configuration: {error}"
            )
            return {}
        for name, field in (
            ("clang", "clang_filepath"),
            ("clang++", "clangxx_filepath"),
            ("rustc", "rustc_filepath"),
        ):
            required = (
                name == "clang"
                or (
                    name == "clang++"
                    and (recipe.cxx or recipe.build_system == "cargo")
                )
                or (name == "rustc" and recipe.build_system == "cargo")
            )
            if required and (name not in tools.tools or field not in data):
                failures.append(
                    f"missing explicit compiler selection evidence: {name} ({field})"
                )
            elif (
                name in tools.tools
                and field in data
                and data[field] != tools.path(name)
            ):
                failures.append(
                    f"scratch configuration selects mismatched {name}"
                )
        for key in (
            "cache_enabled",
            "cache_dir",
            "bitcode_store_path",
            "bitcode_root",
        ):
            data.pop(key, None)
        return data

    for native, wrapped in zip(
        native_commands, wrapped_commands, strict=False
    ):
        left = tuple(normalize(a, native_build) for a in native.argv)
        right = tuple(normalize(a, wrapped_build) for a in wrapped.argv)
        if left != right:
            failures.append(
                f"native/wrapped workload argv differs: {left!r} != {right!r}"
            )
        if normalize(str(native.cwd), native_build) != normalize(
            str(wrapped.cwd), wrapped_build
        ):
            failures.append("native/wrapped working directories differ")
        environments = []
        for command, build, is_wrapped in (
            (native, native_build, False),
            (wrapped, wrapped_build, True),
        ):
            env = dict(command.env)
            if recipe.build_system == "cargo":
                expected = tools.path("rllvm-rustc") if is_wrapped else ""
                if env.get("RUSTC_WRAPPER", "") != expected:
                    failures.append(
                        "Cargo compiler wrapper selection differs from declared mode"
                    )
                env.pop("RUSTC_WRAPPER", None)
                if (
                    "--crate-name" not in command.argv
                    and "build" in command.argv
                ):
                    for option in (
                        "profile.release.codegen-units=1",
                        "profile.release.build-override.codegen-units=1",
                    ):
                        if option not in command.argv:
                            failures.append(
                                f"Cargo configuration missing {option}"
                            )
            for key in ("RLLVM_CONFIG", "RLLVM_CACHE"):
                env.pop(key, None)
            environments.append(
                {k: normalize(v, build) for k, v in env.items()}
            )
        if environments[0] != environments[1]:
            different = sorted(
                k
                for k in environments[0].keys() | environments[1].keys()
                if environments[0].get(k) != environments[1].get(k)
            )
            failures.append(
                f"native/wrapped workload environment differs: {different}"
            )
        if configuration(native) != configuration(wrapped):
            failures.append(
                "native/wrapped capture compiler configuration differs"
            )
    if recipe.build_system == "autotools":
        for build in (native_build, wrapped_build):
            result = validate_autotools_configuration(build)
            failures.extend(result.failures)
            evidence.extend(result.evidence)
    if recipe.build_system == "cargo":
        for label, records in (
            ("native", native_records),
            ("wrapped", wrapped_records),
        ):
            count, problems = cargo_invocation_audit(records)
            failures.extend(f"{label}: {problem}" for problem in problems)
            evidence.append(
                f"{label} observed target/host rustc invocations: {count}"
            )
            evidence.extend(record.stderr for record in records)
    return Check(tuple(failures), tuple(evidence))
