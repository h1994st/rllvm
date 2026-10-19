import json
import os
import sys
from dataclasses import replace
from pathlib import Path
from typing import Any, cast

import pytest
from typer.testing import CliRunner

from benchmarks.cli import app
from benchmarks.fixtures import PreparedFixture
from benchmarks.recipes import get_recipe
from benchmarks.records import read_json, write_json
from benchmarks.runner import RunOptions, RunResult
from benchmarks.tests.validation_support import (
    read_records,
    tiny_cmake_fixture,
)
from benchmarks.toolchains import Toolchain
from benchmarks.workspace import Workspace

runner = CliRunner()


def test_registered_cli_lists_profiles_and_rejects_unknown_profile(
    tmp_path: Path,
) -> None:
    listed = runner.invoke(app, ["profiles"])
    assert listed.exit_code == 0
    assert "nghttp2-c-cmake" in listed.stdout
    assert listed.stdout.count("(default)") == 3
    assert len(listed.stdout.splitlines()) == 9
    output = tmp_path / "must not exist"
    result = runner.invoke(
        app,
        [
            "prepare",
            "--profile",
            "not-a-profile",
            "--examples-root",
            str(tmp_path),
            "--dependencies-root",
            str(tmp_path),
            "--tool-root",
            str(tmp_path),
            "--output",
            str(output),
        ],
    )
    assert result.exit_code != 0
    assert "unknown benchmark profile" in result.stderr
    assert not output.exists()


def test_run_validation_preserves_unrelated_output_directory(
    tmp_path: Path,
) -> None:
    manifest = tmp_path / "prepared.json"
    manifest.write_text("{}")
    output = tmp_path / "unrelated output"
    output.mkdir()
    sentinel = output / "keep me"
    sentinel.write_text("untouched")
    result = runner.invoke(
        app,
        ["run", "--manifest", str(manifest), "--output", str(output)],
    )
    assert result.exit_code != 0
    assert "already exists" in result.stderr
    assert sentinel.read_text() == "untouched"


def test_prepare_reports_missing_tool_without_modifying_inputs(
    tmp_path: Path,
) -> None:
    examples = tmp_path / "example inputs"
    examples.mkdir()
    tools = tmp_path / "empty tools"
    tools.mkdir()
    sentinel = tools / "preserve"
    sentinel.write_text("unchanged")
    output = tmp_path / "must not exist"
    result = runner.invoke(
        app,
        [
            "prepare",
            "--profile",
            "nghttp2-c-cmake",
            "--examples-root",
            str(examples),
            "--dependencies-root",
            str(tmp_path),
            "--tool-root",
            str(tools),
            "--output",
            str(output),
        ],
    )
    assert result.exit_code != 0
    assert "required tool not found" in result.stderr
    assert sentinel.read_text() == "unchanged"
    assert not output.exists()


def test_run_rejects_invalid_counts_before_creating_output(
    tmp_path: Path,
) -> None:
    manifest = tmp_path / "prepared.json"
    manifest.write_text("{}")
    for option in ("--jobs", "--repetitions", "--extraction-repeats"):
        output = tmp_path / option.removeprefix("--")
        result = runner.invoke(
            app,
            [
                "run",
                "--manifest",
                str(manifest),
                option,
                "0",
                "--output",
                str(output),
            ],
        )
        assert result.exit_code != 0
        assert "positive" in result.stderr
        assert not output.exists()
    output = tmp_path / "too many jobs"
    result = runner.invoke(
        app,
        [
            "run",
            "--manifest",
            str(manifest),
            "--jobs",
            str((os.cpu_count() or 1) + 1),
            "--output",
            str(output),
        ],
    )
    assert result.exit_code != 0
    assert "logical CPU" in result.stderr
    assert not output.exists()


@pytest.mark.full
def test_cli_smoke_run_preserves_base_references_and_reports_offline(
    tmp_path: Path,
) -> None:
    prepared = tiny_cmake_fixture(tmp_path / "tiny fixture", spaces=True)
    write_json(prepared.manifest_path, prepared.manifest())
    output = tmp_path / "run output with spaces"
    # The default of two extraction repeats gives repeat 1 its own log pairs.
    result = runner.invoke(
        app,
        [
            "run",
            "--manifest",
            str(prepared.manifest_path),
            "--smoke",
            "--jobs",
            "2",
            "--output",
            str(output),
        ],
    )
    assert result.exit_code == 0, result.stderr
    group = read_json(output / "run-group.json")
    runs = cast(list[dict[str, Any]], group["runs"])
    run_manifest = Path(runs[0]["manifest"])
    root = run_manifest.parent

    # Scheduler checks read the run directory before any report rewrites it.
    manifest = read_json(run_manifest)
    assert manifest["status"] == "valid", manifest
    assert cast(dict[str, Any], manifest["result"])["failed_samples"] == 0
    samples = read_records(root / "samples.jsonl")
    assert len(samples) == 12
    assert all(s["valid"] for s in samples)
    commands = read_records(root / "commands.jsonl")
    log_pairs = [
        (c["measurement"]["stdout"], c["measurement"]["stderr"])
        for c in commands
        if c.get("measurement") is not None
    ]
    assert len(log_pairs) == len(set(log_pairs))
    edits = read_records(root / "operations.jsonl")
    for arm in (
        "native",
        "wrapped-uncached",
        "wrapped-empty-cache",
        "wrapped-primed-cache",
    ):
        build = root / "arms" / arm / "build"
        assert Path((build / "child-cwd").read_text()) == build
        child_env = dict(
            line.split("=", 1)
            for line in (build / "child-environment").read_text().splitlines()
            if "=" in line
        )
        recorded_env = next(
            c["command"]["env"]
            for c in commands
            if c["arm"] == arm and c["phase"] == "configure"
        )
        assert all(child_env[k] == v for k, v in recorded_env.items())
    api = [
        c
        for c in commands
        if c["phase"] == "api-probe"
        and c["command"]["argv"][0].endswith("/version")
    ]
    assert all(
        Path(c["measurement"]["stdout"]).read_text()
        == (
            "version-rllvm-benchmark\n"
            if c["state"] == "edited"
            else "version\n"
        )
        for c in api
    )
    assert (
        prepared.recipe.edit.after
        not in (prepared.source / prepared.recipe.edit.path).read_text()
    )
    prime = [c for c in commands if c["phase"] == "prime-build"]
    clean = [
        c
        for c in commands
        if c["phase"] == "clean-build" and c["arm"] == "wrapped-primed-cache"
    ]
    assert prime[0]["command"] == clean[0]["command"]
    assert all(
        c["sequence"]
        < next(e["sequence"] for e in edits if e["operation"] == "apply-edit")
        for c in prime
    )
    retained = [
        e for e in edits if e["operation"] == "reset" and e.get("retain_cache")
    ]
    assert retained and any(e["cache_bytes_before"] > 0 for e in retained)
    assert all(
        "RLLVM_BENCHMARK_EVENTS" not in c["command"]["env"]
        for c in commands
        if c["category"] == "timed"
    )
    assert all(
        s["gates"]["edited_ir"]["valid"]
        for s in samples
        if s["state"] == "edited" and s["arm"] != "native"
    )
    diagnostics = read_records(root / "diagnostics.jsonl")
    assert diagnostics and all(d["valid"] for d in diagnostics)
    assert diagnostics[1]["summary"]["event_ids"]
    assert set(diagnostics[0]["summary"]["event_ids"]).isdisjoint(
        diagnostics[1]["summary"]["event_ids"]
    )

    assert group["kind"] == "workflow-run-group"
    assert read_json(run_manifest)["status"] == "valid"

    report_root = tmp_path / "offline report"
    report = runner.invoke(
        app,
        [
            "report",
            str(output / "run-group.json"),
            "--output",
            str(report_root),
        ],
    )
    assert report.exit_code == 0, report.stderr
    before = (report_root / "report.md").read_bytes()
    assert "tiny-c-cmake" in before.decode()
    assert (report_root / "samples.csv").is_file()

    # Removing executables demonstrates report regeneration only reads records.
    data = json.loads(run_manifest.read_text())
    data["toolchain"]["tools"] = {}
    run_manifest.write_text(json.dumps(data) + "\n")
    second = tmp_path / "offline report again"
    report = runner.invoke(
        app,
        ["report", str(run_manifest), "--output", str(second)],
    )
    assert report.exit_code == 0, report.stderr
    assert before == (second / "report.md").read_bytes()


def test_run_stops_group_after_interrupted_profile(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    tools = Toolchain(sys.platform, {}, {})
    manifests = []
    for profile in ("first-profile", "second-profile"):
        recipe = replace(
            get_recipe("nghttp2-c-cmake"),
            profile_id=profile,
            required_submodules=(),
        )
        manifest = tmp_path / f"{profile}.json"
        fixture = PreparedFixture(
            profile,
            tmp_path / f"{profile}-source",
            recipe,
            "fixture",
            "fixture",
            {},
            None,
            tools,
            (),
            manifest,
        )
        write_json(manifest, fixture.manifest())
        manifests.append(manifest)

    started = []

    def interrupted_run(
        prepared: PreparedFixture,
        toolchain: Toolchain,
        options: RunOptions,
    ) -> RunResult:
        del toolchain
        started.append(prepared.recipe.profile_id)
        workspace = Workspace.create(options.root)
        write_json(
            workspace.root / "run.json",
            {
                "schema_version": 1,
                "kind": "workflow-run",
                "status": "interrupted",
            },
        )
        return RunResult(
            workspace.root,
            "interrupted",
            1,
            1,
            ("KeyboardInterrupt",),
        )

    monkeypatch.setattr("benchmarks.cli.run_profile", interrupted_run)
    output = tmp_path / "group output"
    result = runner.invoke(
        app,
        [
            "run",
            "--manifest",
            str(manifests[0]),
            "--manifest",
            str(manifests[1]),
            "--smoke",
            "--output",
            str(output),
        ],
    )
    assert result.exit_code == 130
    assert started == ["first-profile"]
    group = read_json(output / "run-group.json")
    assert group["status"] == "interrupted"
    runs = cast(list[dict[str, Any]], group["runs"])
    assert [item["profile"] for item in runs] == ["first-profile"]
    assert Path(runs[0]["manifest"]).is_file()
    unreached = cast(list[dict[str, Any]], group["unreached"])
    assert unreached == [
        {
            "profile": "second-profile",
            "fixture_identity": "second-profile",
            "reason": "not reached: prior profile interrupted",
        }
    ]
    assert not (output / "second-profile").exists()
    assert "interrupted" in result.stderr
