import json
from pathlib import Path
from typing import Any

import pytest

from benchmarks.records import read_records, write_json
from benchmarks.report import ReportError, generate_report
from benchmarks.tests.report_support import saved_run
from benchmarks.workflow_contract import required_sample_gates


def test_report_excludes_invalid_samples_and_retains_raw_evidence(
    tmp_path: Path,
) -> None:
    manifest = saved_run(tmp_path / "run")
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
    first = saved_run(tmp_path / "first")
    kwargs: Any = {field: value}
    second = saved_run(tmp_path / "second", **kwargs)
    with pytest.raises(ReportError, match="incompatible.*fixture-cmake"):
        generate_report((first, second), tmp_path / "report")


def test_report_rejects_unknown_incomplete_and_malformed_records(
    tmp_path: Path,
) -> None:
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run", planned=True)
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
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run")
    (manifest.parent / f"{stream}.jsonl").unlink()
    with pytest.raises(ReportError, match=f"missing {stream} evidence"):
        generate_report((manifest,), tmp_path / "report")


def test_report_accepts_missing_streams_for_early_failed_run(
    tmp_path: Path,
) -> None:
    manifest = saved_run(tmp_path / "run")
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
    first = saved_run(tmp_path / "first")
    typed_kwargs: Any = kwargs
    second = saved_run(tmp_path / "second", **typed_kwargs)
    with pytest.raises(ReportError, match=f"incompatible.*{field}"):
        generate_report((first, second), tmp_path / "report")


def test_report_ignores_transient_load_and_location_only_roots(
    tmp_path: Path,
) -> None:
    first = saved_run(tmp_path / "first")
    second = saved_run(tmp_path / "second")
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
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run")
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
    manifest = saved_run(tmp_path / "run")
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
