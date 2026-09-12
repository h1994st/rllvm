"""Serial, owned build/extraction workflows with conjunctive validity gates.

JSON schema v1: run.json is the final status/index; samples.jsonl contains one
final observation per arm/state/repetition. samples/*.json are incrementally
updated while commands/operations/validations/diagnostics.jsonl are append-only.
No timings from validation, priming, diagnostics or filesystem work enter the
``timed`` category. Disabled diagnostics and dry runs cannot establish validity.
"""

import json
import os
import platform
import random
import re
import time
import traceback
from dataclasses import asdict, dataclass, field, is_dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from benchmarks.coverage import (
    Coverage,
    autotools_coverage,
    cargo_coverage,
    cmake_coverage,
)
from benchmarks.fixtures import PreparedFixture
from benchmarks.probe import prepare_diagnostics, read_events, summarize_events
from benchmarks.process import Command, CommandFailed, Measurement, execute
from benchmarks.recipes import EditRecord, Target
from benchmarks.records import append_record, write_json
from benchmarks.toolchains import Toolchain, sha256
from benchmarks.validation import (
    Check,
    Extraction,
    Validation,
    validate_behavior,
    validate_configuration,
    validate_extraction_set,
    validate_target,
)
from benchmarks.workspace import RunLock, Workspace, allocated_bytes

ARMS = (
    "native",
    "wrapped-uncached",
    "wrapped-empty-cache",
    "wrapped-primed-cache",
)
STATES = ("clean", "unchanged", "edited")


@dataclass(frozen=True)
class RunOptions:
    root: Path
    repetitions: int = 3
    jobs: int = 2
    seed: int = 144
    extraction_repeats: int = 2
    diagnostics: bool = True
    dry_run: bool = False
    rllvm_provenance: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class RunResult:
    root: Path
    status: str
    failed_samples: int
    sample_count: int
    errors: tuple[str, ...]
    schema_version: int = 1

    @property
    def valid(self) -> bool:
        return self.status == "valid" and self.failed_samples == 0


def _json(value: Any) -> Any:
    if is_dataclass(value) and not isinstance(value, type):
        return _json(asdict(value))
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, dict):
        return {str(k): _json(v) for k, v in value.items()}
    if isinstance(value, tuple | list):
        return [_json(v) for v in value]
    return value


def comparison_orders(
    repetitions: int, seed: int
) -> tuple[tuple[str, ...], ...]:
    """Seeded cyclic Latin order: complete blocks balance every arm position."""
    arms = list(ARMS)
    random.Random(seed).shuffle(arms)
    return tuple(
        tuple(arms[i % 4 :] + arms[: i % 4]) for i in range(repetitions)
    )


def _disk(path: Path) -> dict[str, int]:
    seen = set()
    logical = 0
    stack = [path]
    while stack:
        current = stack.pop()
        stat = current.lstat()
        identity = (stat.st_dev, stat.st_ino)
        if identity in seen:
            continue
        seen.add(identity)
        if current.is_dir() and not current.is_symlink():
            stack.extend(current.iterdir())
        else:
            logical += stat.st_size
    return {"logical_bytes": logical, "allocated_bytes": allocated_bytes(path)}


def _totals(records: list[dict]) -> dict[str, dict]:
    result: dict[str, dict] = {}
    for record in records:
        key = record["category"] + ":" + record["phase"]
        group = result.setdefault(
            key,
            {
                "commands": 0,
                "wall_seconds": 0.0,
                "user_cpu_seconds": 0.0,
                "system_cpu_seconds": 0.0,
                "max_observed_process_rss_bytes": 0,
                "complete": True,
                "rss_scope": "maximum observed command high-water; not tree peak",
            },
        )
        group["commands"] += 1
        measurement = record["measurement"]
        if measurement is None:
            group["complete"] = False
            continue
        for name in ("wall_seconds", "user_cpu_seconds", "system_cpu_seconds"):
            if measurement[name] is None:
                group["complete"] = False
            else:
                group[name] += measurement[name]
        rss = measurement["max_process_rss_bytes"]
        if rss is not None:
            group["max_observed_process_rss_bytes"] = max(
                rss, group["max_observed_process_rss_bytes"]
            )
        if measurement["returncode"] != 0:
            group["complete"] = False
    return result


class _Run:
    def __init__(
        self, prepared: PreparedFixture, tools: Toolchain, options: RunOptions
    ):
        self.prepared, self.tools, self.options = prepared, tools, options
        self.recipe = prepared.recipe
        self.root = options.root.absolute()
        self.workspace: Workspace
        self.sequence = 0
        self.commands: list[dict] = []
        self.samples: dict[str, dict] = {}
        self.errors: list[str] = []
        self.arm = "run"
        self.repetition = -1
        self.state = "setup"
        self.edit: EditRecord | None = None
        self.build_records: dict[str, list[Measurement]] = {}
        self.workload_commands: dict[str, tuple[Command, ...]] = {}
        self.behavior: dict[tuple[str, str], tuple[Measurement, ...]] = {}
        self.api: dict[tuple[str, str], tuple[Measurement, ...]] = {}
        self.targets = self.recipe.targets(tools.host)

    def envelope(self, **data) -> dict:
        self.sequence += 1
        return {
            "schema_version": 1,
            "sequence": self.sequence,
            "repetition": self.repetition,
            "arm": self.arm,
            "state": self.state,
            **_json(data),
        }

    def record(self, stream: str, **data) -> dict:
        record = self.envelope(**data)
        append_record(self.root / (stream + ".jsonl"), record)
        return record

    def command(
        self, command: Command, phase: str, category: str = "timed"
    ) -> Measurement | None:
        record = self.envelope(
            phase=phase, category=category, command=command, measurement=None
        )
        # A planned command remains durable even if execute is interrupted.
        self.record(
            "operations",
            operation="command-start",
            command_sequence=record["sequence"],
            command=command,
            phase=phase,
            category=category,
        )
        try:
            measurement = (
                None
                if self.options.dry_run
                else execute(
                    command, self.root / "logs", f"{record['sequence']:08d}"
                )
            )
        except BaseException:
            record["error"] = traceback.format_exc()
            append_record(self.root / "commands.jsonl", record)
            self.commands.append(record)
            raise
        record["measurement"] = _json(measurement)
        append_record(self.root / "commands.jsonl", record)
        self.commands.append(record)
        if measurement is not None and measurement.returncode != 0:
            raise CommandFailed(measurement)
        return measurement

    def invoke(
        self,
        commands: tuple[Command, ...],
        phase: str,
        category: str = "timed",
    ) -> tuple[Measurement, ...]:
        results = []
        for command in commands:
            result = self.command(command, phase, category)
            if result is not None:
                results.append(result)
        return tuple(results)

    def operation(self, name: str, function, **details):
        start = time.monotonic()
        try:
            result = None if self.options.dry_run else function()
        except BaseException:
            self.record(
                "operations",
                operation=name,
                valid=False,
                error=traceback.format_exc(),
                wall_seconds=time.monotonic() - start,
                **details,
            )
            raise
        self.record(
            "operations",
            operation=name,
            valid=True,
            planned=self.options.dry_run,
            result=result,
            wall_seconds=None
            if self.options.dry_run
            else time.monotonic() - start,
            **details,
        )
        return result

    def build(self, arm: str) -> Path:
        return self.root / "arms" / arm / "build"

    def env(self, arm: str) -> dict[str, str]:
        return dict(
            self.tools.environment,
            RLLVM_CONFIG=str(self.root / "arms" / arm / "rllvm.toml"),
            RLLVM_CACHE="1"
            if arm.endswith("cache") and arm != "wrapped-uncached"
            else "0",
        )

    def configure(self, arm: str, tools=None, env=None, build=None):
        return self.recipe.configure_commands(
            self.prepared.source,
            build or self.build(arm),
            tools or self.tools,
            jobs=self.options.jobs,
            env=env or self.env(arm),
            wrapped=arm != "native",
        )

    def builds(self, arm: str, tools=None, env=None, build=None):
        return self.recipe.build_commands(
            self.prepared.source,
            build or self.build(arm),
            tools or self.tools,
            jobs=self.options.jobs,
            env=env or self.env(arm),
            wrapped=arm != "native",
        )

    def setup(self):
        started = time.monotonic()
        for arm in ARMS:
            directory = self.root / "arms" / arm
            directory.mkdir(parents=True)
            settings: dict[str, str | bool] = {
                "cache_enabled": self.env(arm)["RLLVM_CACHE"] == "1",
                "cache_dir": str(directory / "cache"),
                "bitcode_store_path": str(directory / "bitcode"),
            }
            for name, key in (
                ("clang", "clang"),
                ("clang++", "clangxx"),
                ("rustc", "rustc"),
                ("llvm-link", "llvm_link"),
                ("llvm-ar", "llvm_ar"),
                ("llvm-config", "llvm_config"),
                ("llvm-objcopy", "llvm_objcopy"),
            ):
                if name in self.tools.tools:
                    settings[key + "_filepath"] = self.tools.path(name)
            (directory / "rllvm.toml").write_text(
                "\n".join(
                    f"{k} = {json.dumps(v)}" for k, v in settings.items()
                )
                + "\n"
            )
        self.record(
            "operations",
            operation="scratch-configurations",
            wall_seconds=time.monotonic() - started,
            configurations={
                arm: {
                    "path": self.env(arm)["RLLVM_CONFIG"],
                    "sha256": sha256(Path(self.env(arm)["RLLVM_CONFIG"])),
                    "contents": Path(
                        self.env(arm)["RLLVM_CONFIG"]
                    ).read_text(),
                }
                for arm in ARMS
            },
        )

    def reset(self, arm: str, *, retain_cache: bool):
        directory = self.root / "arms" / arm
        cache = directory / "cache"
        before = allocated_bytes(cache) if cache.exists() else 0

        def reset():
            names = ["build", "bitcode", "extractions"]
            if not retain_cache:
                names.append("cache")
            for name in names:
                self.workspace.reset_directory(Path("arms") / arm / name)
            return self.recipe.prepare_build_evidence(self.build(arm))

        self.operation(
            "reset",
            reset,
            retain_cache=retain_cache,
            cache_bytes_before=before,
            arm_path=str(directory),
            paths=[
                str(directory / name)
                for name in ("build", "bitcode", "extractions")
            ]
            + ([] if retain_cache else [str(cache)]),
        )

    def start_sample(self, arm: str, state: str) -> dict:
        self.arm, self.state = arm, state
        identity = f"{self.repetition:03d}-{arm}-{state}"
        sample = self.envelope(
            id=identity,
            valid=False,
            complete=False,
            gates={},
            errors=[],
            command_sequences=[],
            phase_totals={},
            disk={},
            cache_state=(
                "disabled"
                if arm in ("native", "wrapped-uncached")
                else "primed-original"
                if arm == "wrapped-primed-cache"
                else "empty-at-clean-start"
            ),
            build_artifact_state="empty" if state == "clean" else "retained",
            cargo_artifact_state=("empty" if state == "clean" else "retained")
            if self.recipe.build_system == "cargo"
            else None,
        )
        self.samples[identity] = sample
        self.save_sample(sample)
        return sample

    def save_sample(self, sample: dict):
        write_json(self.root / "samples" / (sample["id"] + ".json"), sample)

    def gate(self, sample: dict, name: str, check):
        data = _json(check) | {"valid": check.valid}
        sample["gates"][name] = data
        self.record("validations", sample=sample["id"], gate=name, **data)
        self.save_sample(sample)

    def base_build(self, arm: str):
        sample = self.start_sample(arm, "clean")
        try:
            self.reset(arm, retain_cache=False)
            if arm == "wrapped-primed-cache":
                self.invoke(self.configure(arm), "prime-configure", "priming")
                self.invoke(self.builds(arm), "prime-build", "priming")
                self.reset(arm, retain_cache=True)
            configure = self.configure(arm)
            builds = self.builds(arm)
            self.workload_commands[arm] = configure + builds
            records = self.invoke(configure, "configure")
            records += self.invoke(builds, "clean-build")
            self.build_records[arm] = list(records)
            self.gate(sample, "commands", Check())
        except Exception:
            self.fail(sample)

    def fail(self, sample: dict, gate: str = "commands"):
        error = traceback.format_exc()
        sample["errors"].append(error)
        self.gate(sample, gate, Check((error,)))

    def coverage(self, target: Target, arm: str) -> Coverage:
        args = (target, self.prepared.source, self.build(arm))
        records = tuple(self.build_records.get(arm, ()))
        if self.recipe.build_system == "cmake":
            return cmake_coverage(*args)
        if self.recipe.build_system == "autotools":
            return autotools_coverage(*args, records)
        return cargo_coverage(
            *args, records, project_package=self.recipe.project
        )

    def observations(self, sample: dict):
        arm, state = sample["arm"], sample["state"]
        self.arm, self.state = arm, state
        env, build = self.env(arm), self.build(arm)
        behavior = self.recipe.behavior_commands(
            self.prepared.source,
            build,
            self.tools,
            jobs=self.options.jobs,
            env=env,
            wrapped=arm != "native",
        )
        self.behavior[arm, state] = self.invoke(
            behavior, "behavior", "validation"
        )
        probe_dir = self.root / "probes" / sample["id"]
        self.operation(
            "probe-source",
            lambda: (
                (
                    probe_dir.mkdir(parents=True),
                    (probe_dir / "version.c").write_text(
                        self.recipe.version_probe_source()
                    ),
                )
                and None
            ),
            path=str(probe_dir / "version.c"),
        )
        probes = self.recipe.version_probe_commands(
            self.prepared.source,
            build,
            probe_dir / "version.c",
            probe_dir / "version",
            self.tools,
            env=env,
        )
        self.api[arm, state] = self.invoke(probes, "api-probe", "validation")[
            -1:
        ]

    def extract(
        self, sample: dict, target: Target, label: str
    ) -> Validation | None:
        arm = sample["arm"]
        output = (
            self.root
            / "arms"
            / arm
            / "extractions"
            / (f"{sample['state']}-{label}-{target.id}.bc")
        )
        artifact = self.build(arm) / target.artifact
        result = self.command(
            Command(
                (
                    self.tools.path("rllvm-get-bc"),
                    "--save-manifest",
                    "-o",
                    str(output),
                    str(artifact),
                ),
                self.build(arm),
                self.env(arm),
            ),
            "extract-" + label,
        )
        self.record(
            "operations",
            operation="validate-target",
            target=target.id,
            extraction=str(output),
            planned=self.options.dry_run,
        )
        if self.options.dry_run:
            return None
        start = time.monotonic()
        validation = validate_target(
            target,
            self.build("native") / target.artifact,
            artifact,
            Extraction(
                output, artifact.parent / (output.name + ".manifest"), result
            ),
            self.tools,
            logs=self.root
            / "validation-logs"
            / sample["id"]
            / label
            / target.id,
            env=self.env(arm),
            coverage=self.coverage(target, arm),
        )
        self.record(
            "operations",
            operation="target-validation",
            wall_seconds=time.monotonic() - start,
            target=target.id,
        )
        for measurement in validation.records:
            record = self.record(
                "commands",
                phase="target-validation",
                category="validation",
                measurement=measurement,
                command=Command(
                    measurement.argv, Path(measurement.cwd), self.env(arm)
                ),
            )
            self.commands.append(record)
        self.gate(sample, label + ":" + target.id, validation)
        if target.kind in ("static", "shared") and sample["state"] == "edited":
            expected = (
                Path(self.api["native", "edited"][0].stdout)
                .read_text()
                .strip()
            )
            command = Command(
                (self.tools.path("llvm-dis"), str(output), "-o", "-"),
                self.build(arm),
                self.env(arm),
            )
            ir = self.command(command, "edited-ir", "validation")
            assert ir is not None
            check = version_constant_check(
                Path(ir.stdout).read_text(), expected
            )
            previous = sample["gates"].get(
                "edited_ir", {"failures": [], "evidence": []}
            )
            self.gate(
                sample,
                "edited_ir",
                Check(
                    tuple(previous["failures"]) + check.failures,
                    tuple(previous["evidence"]) + check.evidence,
                ),
            )
        return validation

    def validate(self, sample: dict):
        self.arm, self.state = sample["arm"], sample["state"]
        arm, state = self.arm, self.state
        if not sample["gates"].get("commands", {}).get("valid"):
            return
        try:
            self.observations(sample)
            if self.options.dry_run:
                self.extractions(sample)
                return
            reference = self.samples[f"{self.repetition:03d}-native-{state}"]
            if not reference["gates"].get("commands", {}).get("valid"):
                raise ValueError(
                    "matching native state did not build successfully"
                )
            self.gate(
                sample,
                "configuration",
                validate_configuration(
                    self.recipe,
                    self.workload_commands["native"],
                    self.workload_commands[arm],
                    self.tools,
                    self.build("native"),
                    self.build(arm),
                    native_records=tuple(self.build_records["native"]),
                    wrapped_records=tuple(self.build_records[arm]),
                )
                if arm != "native"
                else Check(),
            )
            for target in self.targets:
                coverage = self.coverage(target, arm)
                exists = (self.build(arm) / target.artifact).is_file()
                self.gate(
                    sample,
                    "coverage:" + target.id,
                    Check(
                        coverage.failures
                        + (
                            ()
                            if coverage.complete and exists
                            else (
                                "missing artifact or incomplete independent coverage",
                            )
                        ),
                        tuple(coverage.evidence),
                    ),
                )
            native_behavior = self.behavior["native", state]
            wrapped_behavior = self.behavior[arm, state]
            behavior_checks = []
            offset = 0
            for target in self.targets:
                count = len(target.validation_argv)
                if count:
                    check = validate_behavior(
                        native_behavior[offset : offset + count],
                        wrapped_behavior[offset : offset + count],
                        kind="munit" if target.id == "tests" else "exact",
                    )
                    behavior_checks.append(check)
                    offset += count
            self.gate(
                sample,
                "behavior",
                Check(
                    tuple(f for c in behavior_checks for f in c.failures),
                    tuple(e for c in behavior_checks for e in c.evidence),
                ),
            )
            self.gate(
                sample,
                "api",
                validate_behavior(
                    self.api["native", state],
                    self.api[arm, state],
                    expected_suffix=self.recipe.edit.expected_suffix
                    if state == "edited"
                    else None,
                ),
            )
            self.extractions(sample)
            self.scan(sample)
        except Exception:
            self.fail(sample, "validation-execution")

    def extractions(self, sample: dict):
        if sample["arm"] == "native":
            return
        selected = next(
            t for t in self.targets if t.id == self.recipe.selected_target_id
        )
        first = self.extract(sample, selected, "selected")
        all_results = {}
        for target in self.targets:
            validation = self.extract(sample, target, "all")
            if validation is not None:
                all_results[target.id] = validation
        if not self.options.dry_run:
            self.gate(
                sample,
                "extraction-set",
                validate_extraction_set(
                    self.targets,
                    all_results,
                    repeated=None,
                ),
            )
            self.gate(
                sample,
                "selected-equivalence",
                validate_extraction_set(
                    (selected,),
                    {selected.id: all_results[selected.id]},
                    repeated={selected.id: first} if first is not None else {},
                ),
            )
        for index in range(self.options.extraction_repeats):
            repeated = {}
            for target in self.targets:
                result = self.extract(sample, target, f"repeat-{index}")
                if result is not None:
                    repeated[target.id] = result
            if not self.options.dry_run:
                self.gate(
                    sample,
                    f"repeat-set:{index}",
                    validate_extraction_set(
                        self.targets,
                        all_results,
                        repeated=repeated,
                    ),
                )
        output = (
            self.root
            / "arms"
            / sample["arm"]
            / "extractions"
            / (f"{sample['state']}-selected-{selected.id}.bc")
        )
        self.command(
            Command(
                (self.tools.path("rllvm-info"), str(output)),
                self.build(sample["arm"]),
                self.env(sample["arm"]),
            ),
            "inspect-merged-module",
        )

    def scan(self, sample: dict):
        root = self.root / "arms" / sample["arm"]

        def scan():
            return {
                name: _disk(root / path)
                for name, path in (
                    ("native-and-build-artifacts", "build"),
                    ("bitcode-store", "bitcode"),
                    ("private-cxx-bitcode-cache", "cache"),
                    ("extractions", "extractions"),
                )
            }

        sample["disk"] = self.operation("disk-scan", scan)

    def incremental(self, arm: str, state: str):
        sample = self.start_sample(arm, state)
        base = self.samples[f"{self.repetition:03d}-{arm}-clean"]
        if not base["gates"].get("commands", {}).get("valid"):
            self.gate(
                sample,
                "commands",
                Check(("clean build failed; dependent state skipped",)),
            )
            return
        try:
            records = self.invoke(self.builds(arm), state + "-build")
            self.build_records[arm].extend(records)
            self.gate(sample, "commands", Check())
        except Exception:
            self.fail(sample)

    def apply_edit(self):
        self.edit = self.operation(
            "apply-edit",
            lambda: self.recipe.edit.apply(self.prepared.source),
            descriptor=self.recipe.edit,
        )

    def restore(self):
        if self.edit is not None:
            edit = self.edit
            self.operation(
                "restore-source",
                lambda: self.recipe.edit.restore(self.prepared.source, edit),
            )
            self.edit = None
        elif self.options.dry_run:
            self.operation("restore-source", lambda: None)

    def diagnostics(self) -> Check:
        self.arm, self.state = "diagnostic", "base"
        if not self.options.diagnostics:
            return Check(
                ("diagnostics disabled; execution health unverified",)
            )
        directory = self.root / "diagnostic" / f"{self.repetition:03d}"
        if self.options.dry_run:
            self.record(
                "operations",
                operation="diagnostic-replays",
                root=str(directory),
                states=["cold", "primed", "unchanged", "edited"],
                planned=True,
            )
            return Check(("dry run: diagnostic evidence unavailable",))
        start = time.monotonic()
        session = prepare_diagnostics(
            directory,
            self.root / "arms/wrapped-primed-cache/cache",
            self.tools,
            env=self.tools.environment,
        )
        self.record(
            "operations",
            operation="diagnostic-preparation",
            wall_seconds=time.monotonic() - start,
            session=str(directory / "session.json"),
        )
        for probe in session.probes:
            record = self.record(
                "commands",
                phase="diagnostic-preparation",
                category="diagnostic",
                measurement=probe.preparation,
                command=Command(
                    probe.preparation.argv,
                    Path(probe.preparation.cwd),
                    self.tools.environment,
                ),
            )
            self.commands.append(record)
        build = directory / "build"
        prior: frozenset[str] = frozenset()
        failures = []
        try:
            for phase in ("cold", "primed", "unchanged", "edited"):
                self.state = phase
                start_index = len(self.commands)
                phase_failures = []
                try:
                    if phase in ("cold", "primed"):
                        self.operation(
                            "diagnostic-reset",
                            lambda: self.workspace.reset_directory(
                                build.relative_to(self.root)
                            ),
                            retain_cache=phase == "primed",
                            path=str(build),
                        )
                        self.operation(
                            "diagnostic-evidence",
                            lambda: self.recipe.prepare_build_evidence(build),
                        )
                        self.invoke(
                            self.configure(
                                "wrapped-empty-cache",
                                session.toolchain,
                                session.environment,
                                build,
                            ),
                            "diagnostic-configure",
                            "diagnostic",
                        )
                    if phase == "edited":
                        self.apply_edit()
                    self.invoke(
                        self.builds(
                            "wrapped-empty-cache",
                            session.toolchain,
                            session.environment,
                            build,
                        ),
                        "diagnostic-build",
                        "diagnostic",
                    )
                    # Extraction tools are instrumented too: retain actual link/ar execs.
                    for target in self.targets:
                        output = directory / f"{phase}-{target.id}.bc"
                        self.command(
                            Command(
                                (
                                    self.tools.path("rllvm-get-bc"),
                                    "--save-manifest",
                                    "-o",
                                    str(output),
                                    str(build / target.artifact),
                                ),
                                build,
                                session.environment,
                            ),
                            "diagnostic-extract",
                            "diagnostic",
                        )
                except Exception:
                    phase_failures.append(traceback.format_exc())
                measurements = tuple(
                    Measurement(**r["measurement"])
                    for r in self.commands[start_index:]
                    if r["measurement"] is not None
                )
                try:
                    events = read_events(session.events)
                    summary = summarize_events(
                        tuple(e for e in events if e.event_id not in prior),
                        measurements=measurements,
                        cache_trace=True,
                        health=session.health,
                        prior_event_ids=prior,
                    )
                    prior = frozenset(e.event_id for e in events)
                    phase_failures.extend(summary.failures)
                    self.record(
                        "diagnostics",
                        phase=phase,
                        summary=summary,
                        valid=summary.valid and not phase_failures,
                        failures=phase_failures,
                        command_sequences=[
                            r["sequence"] for r in self.commands[start_index:]
                        ],
                    )
                except Exception:
                    phase_failures.append(traceback.format_exc())
                    self.record(
                        "diagnostics",
                        phase=phase,
                        valid=False,
                        failures=phase_failures,
                    )
                failures.extend(phase_failures)
        finally:
            self.restore()
        return Check(tuple(failures), (str(directory / "session.json"),))

    def run(self):
        self.setup()
        for repetition, order in enumerate(
            comparison_orders(self.options.repetitions, self.options.seed)
        ):
            self.repetition = repetition
            self.build_records = {}
            self.behavior, self.api = {}, {}
            # Build order is balanced. Validation always starts with native.
            for arm in order:
                self.base_build(arm)
            for arm in ("native", *(a for a in order if a != "native")):
                self.validate(self.samples[f"{repetition:03d}-{arm}-clean"])
            for arm in order:
                self.incremental(arm, "unchanged")
            for arm in ("native", *(a for a in order if a != "native")):
                self.validate(
                    self.samples[f"{repetition:03d}-{arm}-unchanged"]
                )
            try:
                self.apply_edit()
                for arm in order:
                    self.incremental(arm, "edited")
                for arm in ("native", *(a for a in order if a != "native")):
                    self.validate(
                        self.samples[f"{repetition:03d}-{arm}-edited"]
                    )
            finally:
                self.restore()
            try:
                health = self.diagnostics()
            except Exception:
                health = Check((traceback.format_exc(),))
            for sample in self.samples.values():
                if sample["repetition"] == repetition:
                    self.arm, self.state = sample["arm"], sample["state"]
                    self.gate(sample, "diagnostics", health)
                    sample["complete"] = True
                    self.finish(sample)

    def finish(self, sample: dict):
        records = [
            r
            for r in self.commands
            if r["repetition"] == sample["repetition"]
            and r["arm"] == sample["arm"]
            and r["state"] == sample["state"]
        ]
        sample["command_sequences"] = [r["sequence"] for r in records]
        sample["phase_totals"] = _totals(records)
        required = {
            "commands",
            "configuration",
            "behavior",
            "api",
            "diagnostics",
        }
        required |= {"coverage:" + t.id for t in self.targets}
        if sample["arm"] != "native":
            required |= {"extraction-set"}
            required |= {
                f"repeat-set:{i}"
                for i in range(self.options.extraction_repeats)
            }
            if sample["state"] == "edited":
                required.add("edited_ir")
        missing = required - sample["gates"].keys()
        sample["missing_gates"] = sorted(missing)
        sample["valid"] = bool(
            sample["complete"]
            and not missing
            and not sample["errors"]
            and all(g["valid"] for g in sample["gates"].values())
        )
        self.save_sample(sample)


def version_constant_check(ir: str, expected: str) -> Check:
    """Match complete NUL-terminated decoded constants, never metadata names."""
    constants = []
    for match in re.finditer(r'\bc"((?:[^"\\]|\\[0-9A-Fa-f]{2})*)"', ir):
        decoded = re.sub(
            r"\\([0-9a-fA-F]{2})", lambda m: chr(int(m[1], 16)), match[1]
        )
        constants.append(decoded.encode("latin1"))
    expected_bytes = expected.encode() + b"\0"
    if not expected or expected_bytes not in constants:
        return Check(
            (
                f"edited library IR missing exact API version constant: {expected!r}",
            )
        )
    return Check(evidence=("decoded LLVM string constant: " + expected,))


def run_profile(
    prepared: PreparedFixture, toolchain: Toolchain, options: RunOptions
) -> RunResult:
    """Run one profile under the host lock; preserve invalid/interrupted runs.

    root must be new. Reusing a run is refused, preserving its logs. Prepared
    sources are private inputs; their sole semantic edit is hash-checked and
    restored in finally. Known binary build provenance is supplied explicitly.
    """
    runner = _Run(prepared, toolchain, options)
    root = runner.root
    if root.exists():
        raise FileExistsError(f"run output already exists: {root}")
    runner.workspace = Workspace.create(root)
    started = datetime.now(UTC).isoformat()
    manifest = {
        "schema_version": 1,
        "kind": "workflow-run",
        "status": "incomplete",
        "valid": False,
        "started_utc": started,
        "options": _json(options),
        "fixture": prepared.manifest(),
        "toolchain": toolchain.manifest(),
        "rllvm_provenance": options.rllvm_provenance,
        "provenance_note": "unspecified revisions remain unknown; binary hashes identify tools",
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "cpu_count": os.cpu_count(),
            "load_start": os.getloadavg(),
        },
        "comparison_orders": comparison_orders(
            options.repetitions, options.seed
        ),
        "order_method": "seeded cyclic Latin order; partial blocks only approximately balanced",
        "label": "smoke" if options.repetitions == 1 else "repeated",
        "ir_stage": "wrapper-captured translation/crate units merged by llvm-link; no LTO",
        "limitations": [
            "OS filesystem cache is uncontrolled; no cache flush performed",
            "CPU/RSS comes from waited command and reaped descendants; not simultaneous tree peak",
            "private C/C++ bitcode cache and Cargo artifact cache are separate states",
            "Cargo target and host release codegen-units=1",
        ],
        "records": {
            name: name + ".jsonl"
            for name in (
                "commands",
                "operations",
                "validations",
                "diagnostics",
                "samples",
            )
        },
    }
    write_json(root / "run.json", manifest)
    interrupted = False
    try:
        with RunLock():
            if options.repetitions < 1 or not 1 <= options.jobs <= (
                os.cpu_count() or 1
            ):
                raise ValueError(
                    "positive repetitions and bounded positive jobs required"
                )
            if options.extraction_repeats < 1:
                raise ValueError(
                    "at least one repeated extraction is required"
                )
            if not prepared.source.is_dir():
                raise ValueError("prepared source directory is missing")
            if not toolchain.path("rllvm-info"):
                raise ValueError("rllvm-info is required")
            runner.run()
    except BaseException as error:
        interrupted = isinstance(error, KeyboardInterrupt | SystemExit)
        runner.errors.append(traceback.format_exc())
    finally:
        # run() owns normal restoration inside the lock. This guard catches
        # an edit followed by an exception before its inner finally was entered.
        if runner.edit is not None:
            try:
                runner.restore()
            except BaseException:
                runner.errors.append(traceback.format_exc())
        for repetition in range(max(0, options.repetitions)):
            runner.repetition = repetition
            for arm in ARMS:
                for state in STATES:
                    identity = f"{repetition:03d}-{arm}-{state}"
                    if identity not in runner.samples:
                        sample = runner.start_sample(arm, state)
                        sample["errors"].append(
                            "not reached: run failed or was interrupted before this sample"
                        )
        for sample in runner.samples.values():
            runner.finish(sample)
            append_record(root / "samples.jsonl", sample)
        count = len(runner.samples)
        failed = sum(not s["valid"] for s in runner.samples.values())
        expected = options.repetitions * len(ARMS) * len(STATES)
        status = (
            "interrupted"
            if interrupted
            else "planned"
            if options.dry_run and not runner.errors
            else "valid"
            if not runner.errors and failed == 0 and count == expected
            else "invalid"
        )
        result = RunResult(root, status, failed, count, tuple(runner.errors))
        manifest.update(
            status=status,
            valid=result.valid,
            result=_json(result),
            expected_samples=expected,
            phase_totals=_totals(runner.commands),
            ended_utc=datetime.now(UTC).isoformat(),
            errors=runner.errors,
        )
        manifest["host"]["load_end"] = os.getloadavg()
        write_json(root / "run.json", manifest)
    return result
