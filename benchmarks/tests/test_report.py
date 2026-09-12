import json
from pathlib import Path
from typing import Any

import pytest

from benchmarks.records import append_record, write_json
from benchmarks.report import ReportError, generate_report


def _run(
    root: Path,
    *,
    source: str = "source-a",
    compiler: str = "compiler-a",
    flags: tuple[str, ...] = ("-DFEATURE=ON",),
    targets: tuple[str, ...] = ("static",),
    jobs: int = 2,
    wrapped_cache_state: str = "disabled",
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
                            "complete": True,
                            "rss_scope": "maximum observed command high-water; not tree peak",
                        },
                        "validation:api": {
                            "commands": 1,
                            "wall_seconds": 0.25,
                            "user_cpu_seconds": 0.1,
                            "system_cpu_seconds": 0.0,
                            "max_observed_process_rss_bytes": 512,
                            "complete": True,
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
    invalid.update(id="invalid", repetition=3, valid=False)
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
    for sample in samples:
        append_record(root / "samples.jsonl", sample)
    append_record(
        root / "diagnostics.jsonl",
        {
            "schema_version": 1,
            "phase": "cold",
            "valid": False,
            "failures": ["compiler count unavailable"],
        },
    )
    for name in ("commands", "operations", "validations"):
        (root / f"{name}.jsonl").write_text("")
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
        "environment": {},
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
            "failed_samples": 1,
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
