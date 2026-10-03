import json
import os
import subprocess
import tarfile
from pathlib import Path

import pytest
from typer.testing import CliRunner

from benchmarks.cli import app
from benchmarks.records import write_json
from benchmarks.report import generate_report
from benchmarks.tests.report_support import saved_run
from benchmarks.toolchains import sha256

runner = CliRunner()
CONDITIONS = "- Fixture host, not dedicated; AC power."


def _git(checkout: Path, *args: str) -> str:
    return subprocess.check_output(
        ("git", *args),
        cwd=checkout,
        env=dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull),
        text=True,
    ).strip()


def _inputs(tmp_path: Path) -> dict[str, Path]:
    (tmp_path / "runs").mkdir()
    manifest = saved_run(
        tmp_path / "runs" / "fixture-cmake",
        wrapped_gate_evidence={
            "extraction-set": {"per_target_module_count": {"static": 3}},
            "all:static": {"native_definitions": ["first", "second"]},
        },
    )
    group = tmp_path / "runs" / "run-group.json"
    write_json(
        group,
        {
            "schema_version": 1,
            "kind": "workflow-run-group",
            "status": "invalid",
            "options": {"repetitions": 3, "jobs": 2, "seed": 144},
            "runs": [
                {
                    "profile": "fixture-cmake",
                    "fixture_identity": "source-a",
                    "manifest": str(manifest),
                    "status": "invalid",
                    "valid": False,
                }
            ],
            "unreached": [],
        },
    )
    report = generate_report((group,), tmp_path / "report").json
    prepared = tmp_path / "prepared-group.json"
    write_json(
        prepared,
        {
            "schema_version": 1,
            "kind": "prepared-fixture-group",
            "profiles": ["fixture-cmake"],
            "manifests": [
                {
                    "profile": "fixture-cmake",
                    "identity": "source-a",
                    "manifest": str(tmp_path / "unused.json"),
                }
            ],
        },
    )
    checkout = tmp_path / "examples" / "fixture"
    checkout.mkdir(parents=True)
    _git(checkout, "init", "-q")
    _git(checkout, "config", "user.name", "Fixture")
    _git(checkout, "config", "user.email", "fixture@example.invalid")
    _git(checkout, "commit", "--allow-empty", "-qm", "fixture")
    load = tmp_path / "load-start.txt"
    load.write_text("load averages: 1.00 1.00 1.00\n")
    return {
        "group": group,
        "manifest": manifest,
        "report": report,
        "prepared": prepared,
        "examples": tmp_path / "examples",
        "checkout": checkout,
        "load": load,
    }


def _package(inputs: dict[str, Path], output: Path):
    return runner.invoke(
        app,
        [
            "baseline",
            str(inputs["group"]),
            "--report",
            str(inputs["report"].parent),
            "--prepared",
            str(inputs["prepared"]),
            "--examples-root",
            str(inputs["examples"]),
            "--conditions",
            CONDITIONS,
            "--context",
            str(inputs["load"]),
            "--output",
            str(output),
        ],
    )


def test_baseline_summarizes_saved_records_and_explains_gaps(
    tmp_path: Path,
) -> None:
    inputs = _inputs(tmp_path)
    output = tmp_path / "baseline"
    result = _package(inputs, output)
    assert result.exit_code == 0, result.stderr

    summary = json.loads((output / "summary.json").read_text())
    profile = summary["profiles"]["fixture-cmake"]
    native = profile["build"]["clean"]["native"]
    wrapped = profile["build"]["clean"]["wrapped-uncached"]
    assert native["raw_wall_seconds"] == [2.0, 4.0, 6.0]
    assert native["median_paired_ratio"] is None
    assert wrapped["median_wall_seconds"] == 8.0
    assert wrapped["median_paired_ratio"] == 2.0
    assert profile["workflow"]["clean"]["wrapped-uncached"] == wrapped
    assert profile["valid_samples"] == 6
    assert profile["coverage"] == {
        "static": {
            "module_counts": [3],
            "native_project_definition_counts": [2],
        }
    }
    assert len(profile["diagnostics"]) == 12
    diagnostic = profile["diagnostics"][0]["summary"]
    assert diagnostic["validated_event_count"] == 2
    assert "event_ids" not in diagnostic
    assert profile["started_utc"] is None
    assert set(profile["unavailable"]) == {
        "started_utc",
        "ended_utc",
        "rllvm_provenance",
    }
    archive = output / "records.tar.xz"
    assert summary["records_archive"]["sha256"] == sha256(archive)
    assert summary["records_archive"]["bytes"] == archive.stat().st_size

    provenance = json.loads((output / "provenance.json").read_text())
    assert provenance["revision"] is None
    assert "revision" in provenance["unavailable"]
    assert provenance["tools"]["clang"]["sha256"] == "compiler-a"

    evidence = json.loads((output / "evidence-manifest.json").read_text())
    stream = inputs["manifest"].parent / "samples.jsonl"
    assert evidence["files"]["runs/fixture-cmake/samples.jsonl"] == {
        "bytes": stream.stat().st_size,
        "sha256": sha256(stream),
    }
    assert "context/load-start.txt" in evidence["files"]

    audit = json.loads((output / "source-preservation.json").read_text())
    assert audit["projects"]["fixture"] == {
        "commit": _git(inputs["checkout"], "rev-parse", "HEAD"),
        "status_porcelain": "",
    }

    readme = (output / "README.md").read_text()
    assert CONDITIONS in readme
    assert "6 of 7 samples passed" in readme
    assert (
        "| fixture-cmake | 4.000 s | 8.000 s (2.00×) | unavailable | "
        "unavailable |"
    ) in readme
    head = _git(inputs["checkout"], "rev-parse", "HEAD")
    assert f"fixture at `{head}` rather than its pin" in " ".join(
        readme.split()
    )

    unpacked = tmp_path / "unpacked"
    with tarfile.open(archive) as tar:
        tar.extractall(unpacked, filter="data")
    again = generate_report((unpacked / "runs.json",), tmp_path / "again")
    assert again.json.read_bytes() == inputs["report"].read_bytes()


def _drop_diagnostics(inputs: dict[str, Path]) -> None:
    (inputs["manifest"].parent / "diagnostics.jsonl").unlink()


def _drop_report_sample(inputs: dict[str, Path]) -> None:
    report = json.loads(inputs["report"].read_text())
    report["samples"].pop()
    inputs["report"].write_text(json.dumps(report))


def _drop_report(inputs: dict[str, Path]) -> None:
    inputs["report"].unlink()


@pytest.mark.parametrize(
    "remove", [_drop_diagnostics, _drop_report_sample, _drop_report]
)
def test_baseline_fails_instead_of_inventing_missing_records(
    tmp_path: Path, remove
) -> None:
    inputs = _inputs(tmp_path)
    remove(inputs)
    output = tmp_path / "baseline"
    result = _package(inputs, output)
    assert result.exit_code == 1
    assert result.stderr.startswith("error: ")
    assert not output.exists()
    assert not list(tmp_path.glob(".baseline.*"))
