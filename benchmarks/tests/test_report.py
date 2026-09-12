import json
from pathlib import Path
from typing import Any

import pytest

from benchmarks.records import append_record, read_records, write_json
from benchmarks.report import ReportError, generate_report
from benchmarks.workflow_contract import (
    DIAGNOSTIC_PHASES,
    required_sample_gates,
)


def _run(
    root: Path,
    *,
    source: str = "source-a",
    compiler: str = "compiler-a",
    flags: tuple[str, ...] = ("-DFEATURE=ON",),
    targets: tuple[str, ...] = ("static",),
    jobs: int = 2,
    wrapped_cache_state: str = "disabled",
    host_machine: str = "x86_64",
    host_processor: str = "fixture-cpu",
    host_identity: str = "fixture-host-a",
    sdkroot: str = "/sdk/one",
    planned: bool = False,
) -> Path:
    root.mkdir()
    samples: list[dict[str, Any]] = []
    for repetition, (native, wrapped) in enumerate(
        zip((2.0, 4.0, 6.0), (4.0, 8.0, 12.0), strict=True)
    ):
        for arm, wall in (("native", native), ("wrapped-uncached", wrapped)):
            samples.append(
                {
                    "schema_version": 1,
                    "id": f"{repetition:03d}-{arm}-clean",
                    "repetition": repetition,
                    "arm": arm,
                    "state": "clean",
                    "valid": not planned,
                    "complete": True,
                    "gates": {},
                    "errors": [],
                    "missing_gates": [],
                    "command_sequences": [repetition],
                    "phase_totals": {
                        "timed:clean-build": {
                            "commands": 1,
                            "wall_seconds": wall,
                            "user_cpu_seconds": wall / 2,
                            "system_cpu_seconds": 0.0,
                            "max_observed_process_rss_bytes": 1024,
                            "complete": not planned,
                            "rss_scope": "maximum observed command high-water; not tree peak",
                        },
                        "validation:api": {
                            "commands": 1,
                            "wall_seconds": 0.25,
                            "user_cpu_seconds": 0.1,
                            "system_cpu_seconds": 0.0,
                            "max_observed_process_rss_bytes": 512,
                            "complete": not planned,
                            "rss_scope": "maximum observed command high-water; not tree peak",
                        },
                    },
                    "disk": {
                        "native-and-build-artifacts": {
                            "logical_bytes": 10,
                            "allocated_bytes": 16,
                        },
                        "bitcode-store": {
                            "logical_bytes": 0 if arm == "native" else 20,
                            "allocated_bytes": 0 if arm == "native" else 24,
                        },
                    },
                    "cache_state": (
                        "disabled" if arm == "native" else wrapped_cache_state
                    ),
                    "build_artifact_state": "empty",
                    "cargo_artifact_state": None,
                }
            )
    invalid = dict(samples[-1])
    invalid.update(id="invalid", repetition=2, valid=False)
    phase_totals = samples[-1]["phase_totals"]
    assert isinstance(phase_totals, dict)
    clean_build = phase_totals["timed:clean-build"]
    assert isinstance(clean_build, dict)
    invalid["phase_totals"] = {
        "timed:clean-build": {
            **clean_build,
            "wall_seconds": 0.01,
        }
    }
    invalid["errors"] = ["validation failed"]
    samples.append(invalid)
    for sequence, sample in enumerate(samples, start=100):
        required = required_sample_gates(
            sample["arm"], sample["state"], targets, 2
        )
        gate_names = {"commands", "diagnostics"} if planned else required
        sample["command_sequences"] = [sequence]
        sample["gates"] = {
            name: {
                "valid": not (planned and name == "diagnostics"),
                "failures": ["dry run: diagnostic evidence unavailable"]
                if planned and name == "diagnostics"
                else [],
                "evidence": [],
            }
            for name in gate_names
        }
        sample["missing_gates"] = sorted(required - gate_names)
        append_record(root / "samples.jsonl", sample)
        append_record(
            root / "commands.jsonl",
            {
                "schema_version": 1,
                "sequence": sequence,
                "repetition": sample["repetition"],
                "arm": sample["arm"],
                "state": sample["state"],
                "phase": "clean-build",
                "category": "timed",
                "command": {"argv": ["build"], "cwd": "/build", "env": {}},
                "measurement": None
                if planned
                else {
                    "returncode": 0,
                    "wall_seconds": sample["phase_totals"][
                        "timed:clean-build"
                    ]["wall_seconds"],
                    "user_cpu_seconds": 1.0,
                    "system_cpu_seconds": 0.0,
                    "max_process_rss_bytes": 1024,
                    "resource_method": "fixture",
                    "failure": None,
                },
            },
        )
        append_record(
            root / "operations.jsonl",
            {
                "schema_version": 1,
                "sequence": sequence + 1000,
                "operation": "command-start",
                "command_sequence": sequence,
            },
        )
        for gate, evidence in sample["gates"].items():
            append_record(
                root / "validations.jsonl",
                {
                    "schema_version": 1,
                    "sequence": sequence + 2000,
                    "sample": sample["id"],
                    "gate": gate,
                    **evidence,
                },
            )
    if not planned:
        for repetition in range(3):
            for phase in DIAGNOSTIC_PHASES:
                append_record(
                    root / "diagnostics.jsonl",
                    {
                        "schema_version": 1,
                        "repetition": repetition,
                        "phase": phase,
                        "valid": True,
                        "failures": [],
                        "summary": {
                            "schema_version": 1,
                            "failures": [],
                            "unobserved": {
                                "rustc": "compiler count unavailable"
                            },
                        },
                    },
                )
    recipe = {
        "profile_id": "fixture-cmake",
        "project": "fixture",
        "build_system": "cmake",
        "repository_url": "https://example.invalid/fixture",
        "commit": source,
        "required_submodules": [],
        "cxx": False,
        "cmake_flags": list(flags),
        "autotools_flags": [],
        "build_targets": list(targets),
        "selected_target_id": targets[0],
        "edit": {"path": "version.c", "before": "a", "after": "b"},
        "lockfile": None,
    }
    toolchain = {
        "host": "linux",
        "tools": {
            "clang": {
                "path": "/tools/clang",
                "realpath": "/tools/clang",
                "sha256": compiler,
                "version": "clang 21",
            }
        },
        "environment": {
            "PATH": "/usr/bin",
            "SDKROOT": sdkroot,
            "DEVELOPER_DIR": "/developer/one",
            "RLLVM_CONFIG": str(root / "discovery/rllvm.toml"),
        },
        "dependencies": [],
        "records": [],
        "generator": "Ninja",
        "rust_host": None,
        "limitations": [],
    }
    fixture = {
        "schema_version": 1,
        "kind": "prepared-fixture",
        "identity": source,
        "source": "/private/output-dependent/source",
        "recipe": recipe,
        "commit": source,
        "tree": source,
        "submodules": {},
        "lock_sha256": None,
        "toolchain": toolchain,
        "preparation_records": [],
        "manifest_path": "/private/output-dependent/prepared.json",
        "targets": [{"id": target} for target in targets],
        "limitations": [],
    }
    manifest = {
        "schema_version": 1,
        "kind": "workflow-run",
        "status": "planned" if planned else "invalid",
        "valid": False,
        "options": {
            "root": str(root),
            "repetitions": 3,
            "jobs": jobs,
            "seed": 144,
            "extraction_repeats": 2,
            "diagnostics": True,
            "dry_run": planned,
            "rllvm_provenance": {},
        },
        "fixture": fixture,
        "toolchain": toolchain,
        "rllvm_provenance": {},
        "host": {
            "platform": "Linux-6.0",
            "machine": host_machine,
            "processor": host_processor,
            "cpu_count": 8,
            "stable_hardware": host_identity,
            "load_start": [1.0, 1.0, 1.0],
            "load_end": [2.0, 2.0, 2.0],
        },
        "limitations": ["filesystem cache uncontrolled"],
        "records": {
            "commands": "commands.jsonl",
            "operations": "operations.jsonl",
            "validations": "validations.jsonl",
            "samples": "samples.jsonl",
            "diagnostics": "diagnostics.jsonl",
        },
        "expected_samples": len(samples),
        "phase_totals": {
            "priming:prime-build": {
                "wall_seconds": 1.0,
                "complete": True,
            }
        },
        "result": {
            "root": str(root),
            "status": "planned" if planned else "invalid",
            "failed_samples": len(samples) if planned else 1,
            "sample_count": len(samples),
            "errors": [],
            "schema_version": 1,
        },
        "errors": [],
    }
    write_json(root / "run.json", manifest)
    return root / "run.json"


def test_report_excludes_invalid_samples_and_retains_raw_evidence(
    tmp_path: Path,
) -> None:
    manifest = _run(tmp_path / "run")
    artifacts = generate_report((manifest,), tmp_path / "report")
    markdown = artifacts.markdown.read_text()
    assert "2, 4, 6" in markdown
    assert "4, 8, 12" in markdown
    assert "4" in markdown and "8" in markdown and "2x" in markdown
    assert "0.01" in markdown
    assert "excluded" in markdown
    assert "validation failed" in markdown
    assert "compiler count unavailable" in markdown
    assert "maximum observed command high-water; not tree peak" in markdown
    assert "native-and-build-artifacts" in markdown
    assert "bitcode-store" in markdown
    assert "validation:api" in markdown
    assert "priming:prime-build" in markdown
    assert "project-owned" in markdown
    assert artifacts.csv.read_text().count("invalid") >= 1
    structured = json.loads(artifacts.json.read_text())
    summaries = {
        item["treatment"]: item
        for item in structured["summaries"]
        if item["state"] == "clean"
    }
    assert summaries["native"]["median_timed_wall_seconds"] == 4
    assert summaries["wrapped-uncached"]["median_timed_wall_seconds"] == 8
    assert summaries["wrapped-uncached"]["median_paired_overhead_ratio"] == 2
    first = (artifacts.markdown.read_bytes(), artifacts.csv.read_bytes())
    again = generate_report((manifest,), tmp_path / "again")
    assert first == (again.markdown.read_bytes(), again.csv.read_bytes())


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("source", "source-b"),
        ("compiler", "compiler-b"),
        ("flags", ("-DFEATURE=OFF",)),
        ("targets", ("shared",)),
        ("jobs", 4),
        ("wrapped_cache_state", "unexpected-cache-treatment"),
    ],
)
def test_report_rejects_mismatched_workloads(
    tmp_path: Path, field: str, value: object
) -> None:
    first = _run(tmp_path / "first")
    kwargs: Any = {field: value}
    second = _run(tmp_path / "second", **kwargs)
    with pytest.raises(ReportError, match="incompatible.*fixture-cmake"):
        generate_report((first, second), tmp_path / "report")


def test_report_rejects_unknown_incomplete_and_malformed_records(
    tmp_path: Path,
) -> None:
    manifest = _run(tmp_path / "run")
    data = json.loads(manifest.read_text())
    data["schema_version"] = 2
    manifest.write_text(json.dumps(data) + "\n")
    with pytest.raises(ReportError, match="unsupported schema version"):
        generate_report((manifest,), tmp_path / "schema")

    data["schema_version"] = 1
    data["status"] = "incomplete"
    write_json(manifest, data)
    with pytest.raises(ReportError, match="incomplete"):
        generate_report((manifest,), tmp_path / "incomplete")

    data["status"] = "invalid"
    write_json(manifest, data)
    with (manifest.parent / "samples.jsonl").open("ab") as stream:
        stream.write(b'{"schema_version":1')
    with pytest.raises(ReportError, match="incomplete final record"):
        generate_report((manifest,), tmp_path / "malformed")


def test_planned_runs_have_no_ratios(tmp_path: Path) -> None:
    manifest = _run(tmp_path / "run", planned=True)
    report = generate_report((manifest,), tmp_path / "report")
    markdown = report.markdown.read_text()
    assert "planned dry run" in markdown
    assert "2.000x" not in markdown
    assert "unavailable (planned dry run)" in markdown


@pytest.mark.parametrize(
    "inconsistency", ["complete", "errors", "missing", "gate"]
)
def test_report_rejects_inconsistent_sample_validity(
    tmp_path: Path, inconsistency: str
) -> None:
    manifest = _run(tmp_path / "run")
    samples = read_records(manifest.parent / "samples.jsonl")
    if inconsistency == "complete":
        samples[0]["complete"] = False
    elif inconsistency == "errors":
        samples[0]["errors"] = ["retained failure"]
    elif inconsistency == "missing":
        samples[0]["missing_gates"] = ["diagnostics"]
    else:
        gates = samples[0]["gates"]
        assert isinstance(gates, dict)
        diagnostic = gates["diagnostics"]
        assert isinstance(diagnostic, dict)
        diagnostic.update(valid=False, failures=["diagnostic failure"])
    (manifest.parent / "samples.jsonl").write_text(
        "".join(json.dumps(sample) + "\n" for sample in samples)
    )
    with pytest.raises(ReportError, match="inconsistent sample validity"):
        generate_report((manifest,), tmp_path / "report")


@pytest.mark.parametrize(
    "stream", ["commands", "operations", "validations", "diagnostics"]
)
def test_report_rejects_missing_reached_evidence(
    tmp_path: Path, stream: str
) -> None:
    manifest = _run(tmp_path / "run")
    (manifest.parent / f"{stream}.jsonl").unlink()
    with pytest.raises(ReportError, match=f"missing {stream} evidence"):
        generate_report((manifest,), tmp_path / "report")


def test_report_accepts_missing_streams_for_early_failed_run(
    tmp_path: Path,
) -> None:
    manifest = _run(tmp_path / "run")
    samples = read_records(manifest.parent / "samples.jsonl")
    for sample in samples:
        arm, state = sample["arm"], sample["state"]
        assert isinstance(arm, str) and isinstance(state, str)
        required = required_sample_gates(arm, state, ("static",), 2)
        sample.update(
            valid=False,
            complete=False,
            gates={},
            errors=["not reached: preflight failed"],
            missing_gates=sorted(required),
            command_sequences=[],
            phase_totals={},
            disk={},
        )
    (manifest.parent / "samples.jsonl").write_text(
        "".join(json.dumps(sample) + "\n" for sample in samples)
    )
    for stream in ("commands", "operations", "validations", "diagnostics"):
        (manifest.parent / f"{stream}.jsonl").unlink()
    run = json.loads(manifest.read_text())
    run["result"]["failed_samples"] = len(samples)
    write_json(manifest, run)
    report = generate_report((manifest,), tmp_path / "report")
    assert "evidence stream" in report.markdown.read_text()


@pytest.mark.parametrize(
    ("field", "kwargs"),
    [
        ("host", {"host_machine": "aarch64", "host_processor": "other"}),
        ("host:stable_hardware", {"host_identity": "fixture-host-b"}),
        ("SDKROOT", {"sdkroot": "/sdk/two"}),
    ],
)
def test_report_rejects_host_and_environment_mismatches(
    tmp_path: Path, field: str, kwargs: dict[str, str]
) -> None:
    first = _run(tmp_path / "first")
    typed_kwargs: Any = kwargs
    second = _run(tmp_path / "second", **typed_kwargs)
    with pytest.raises(ReportError, match=f"incompatible.*{field}"):
        generate_report((first, second), tmp_path / "report")


def test_report_ignores_transient_load_and_location_only_roots(
    tmp_path: Path,
) -> None:
    first = _run(tmp_path / "first")
    second = _run(tmp_path / "second")
    data = json.loads(second.read_text())
    data["host"]["load_start"] = [99.0, 98.0, 97.0]
    data["host"]["load_end"] = [96.0, 95.0, 94.0]
    write_json(second, data)
    report = generate_report((first, second), tmp_path / "report")
    structured = json.loads(report.json.read_text())
    native = next(
        item
        for item in structured["summaries"]
        if item["treatment"] == "native"
    )
    assert native["raw_timed_wall_seconds"] == [2, 4, 6, 2, 4, 6]


def test_report_refuses_dangling_artifact_symlink(tmp_path: Path) -> None:
    manifest = _run(tmp_path / "run")
    output = tmp_path / "report"
    output.mkdir()
    external = tmp_path / "must-not-be-created"
    artifact = output / "report.md"
    artifact.symlink_to(external)
    with pytest.raises(ReportError, match="artifact already exists"):
        generate_report((manifest,), output)
    assert artifact.is_symlink()
    assert not external.exists()


def test_report_rejects_deleted_required_gate(tmp_path: Path) -> None:
    manifest = _run(tmp_path / "run")
    samples = read_records(manifest.parent / "samples.jsonl")
    assert samples[0]["valid"] and samples[0]["complete"]
    assert samples[0]["missing_gates"] == []
    gates = samples[0]["gates"]
    assert isinstance(gates, dict)
    gates.pop("configuration")
    (manifest.parent / "samples.jsonl").write_text(
        "".join(json.dumps(sample) + "\n" for sample in samples)
    )
    with pytest.raises(
        ReportError, match="missing required gate.*configuration"
    ):
        generate_report((manifest,), tmp_path / "report")


@pytest.mark.parametrize(
    "contradiction", ["missing-phases", "failed-phase", "failed-summary"]
)
def test_report_rejects_incomplete_or_failed_diagnostics(
    tmp_path: Path, contradiction: str
) -> None:
    manifest = _run(tmp_path / "run")
    diagnostics = read_records(manifest.parent / "diagnostics.jsonl")
    if contradiction == "missing-phases":
        diagnostics = [
            record
            for record in diagnostics
            if record["repetition"] != 0 or record["phase"] == "cold"
        ]
    elif contradiction == "failed-phase":
        record = next(
            item
            for item in diagnostics
            if item["repetition"] == 0 and item["phase"] == "primed"
        )
        record.update(valid=False, failures=["saved diagnostic invalid"])
    else:
        record = next(
            item
            for item in diagnostics
            if item["repetition"] == 0 and item["phase"] == "primed"
        )
        summary = record["summary"]
        assert isinstance(summary, dict)
        summary["failures"] = ["provider failure"]
    (manifest.parent / "diagnostics.jsonl").write_text(
        "".join(json.dumps(record) + "\n" for record in diagnostics)
    )
    with pytest.raises(ReportError, match="diagnostics evidence"):
        generate_report((manifest,), tmp_path / "report")


@pytest.mark.parametrize(
    "contradiction", ["missing", "failed", "spawn-failed"]
)
def test_report_rejects_unsuccessful_command_for_valid_sample(
    tmp_path: Path, contradiction: str
) -> None:
    manifest = _run(tmp_path / "run")
    commands = read_records(manifest.parent / "commands.jsonl")
    measurement = commands[0]["measurement"]
    assert isinstance(measurement, dict)
    if contradiction == "missing":
        commands[0]["measurement"] = None
    elif contradiction == "failed":
        measurement["returncode"] = 1
    else:
        measurement.update(
            returncode=None,
            failure={
                "kind": "spawn-failed",
                "exception_type": "FileNotFoundError",
                "message": "compiler missing",
                "errno": 2,
            },
        )
    (manifest.parent / "commands.jsonl").write_text(
        "".join(json.dumps(command) + "\n" for command in commands)
    )
    with pytest.raises(ReportError, match="valid sample.*command evidence"):
        generate_report((manifest,), tmp_path / "report")


def test_report_keeps_failed_command_for_invalid_sample_readable(
    tmp_path: Path,
) -> None:
    manifest = _run(tmp_path / "run")
    commands = read_records(manifest.parent / "commands.jsonl")
    measurement = commands[-1]["measurement"]
    assert isinstance(measurement, dict)
    measurement["returncode"] = 1
    (manifest.parent / "commands.jsonl").write_text(
        "".join(json.dumps(command) + "\n" for command in commands)
    )
    report = generate_report((manifest,), tmp_path / "report")
    structured = json.loads(report.json.read_text())
    invalid = next(
        sample
        for sample in structured["samples"]
        if sample["sample_id"] == "invalid"
    )
    assert not invalid["valid"]
    assert invalid["timed_wall_seconds"] == 0.01
