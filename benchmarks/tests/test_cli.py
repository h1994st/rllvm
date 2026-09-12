import json
import os
from dataclasses import replace
from pathlib import Path
from typing import Any, cast

from typer.testing import CliRunner

from benchmarks.cli import app
from benchmarks.fixtures import PreparedFixture
from benchmarks.recipes import Edit, Recipe
from benchmarks.records import read_json, write_json
from benchmarks.tests.validation_support import commit_fixture, tools_at
from benchmarks.toolchains import Toolchain

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


def _tiny(tmp_path: Path) -> PreparedFixture:
    root = tmp_path / "tiny fixture"
    root.mkdir()
    discovered = tools_at(root)
    extra = Toolchain.discover(
        ("llvm-ranlib", "rllvm-info"),
        root / "extra tools",
        paths={
            "llvm-ranlib": Path(discovered.path("llvm-ar")).with_name(
                "llvm-ranlib"
            ),
            "rllvm-info": Path(discovered.path("rllvm-cc")).with_name(
                "rllvm-info"
            ),
        },
    )
    tools = replace(discovered, tools=discovered.tools | extra.tools)
    source = root / "source with spaces"
    (source / "lib/includes/tiny").mkdir(parents=True)
    (source / "tests").mkdir()
    (source / "lib/includes/tiny/tiny.h").write_text(
        "struct tiny_info { const char *version_str; };\n"
        "const struct tiny_info *tiny_version(int);\n"
    )
    (source / "lib/version.c").write_text(
        '#include "tiny/tiny.h"\n'
        'static const struct tiny_info info = {"version"};\n'
        "const struct tiny_info *tiny_version(int unused){return &info;}\n"
    )
    (source / "tests/main.cpp").write_text(
        '#include <cstdio>\nextern "C" {\n#include "tiny/tiny.h"\n}\n'
        'int main(){puts(tiny_version(0)->version_str);puts("/tiny [ OK ]\\n'
        '1 of 1 (100%) tests successful, 0 (0%) test skipped.");return 0;}\n'
    )
    (source / "CMakeLists.txt").write_text(
        "cmake_minimum_required(VERSION 3.20)\n"
        "project(tiny LANGUAGES C CXX)\n"
        "include_directories(lib/includes)\n"
        "add_library(tiny SHARED lib/version.c)\n"
        "add_library(tiny_static STATIC lib/version.c)\n"
        "set_target_properties(tiny tiny_static PROPERTIES "
        "OUTPUT_NAME tiny ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib "
        "LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)\n"
        "add_executable(main tests/main.cpp)\n"
        "target_link_libraries(main PRIVATE tiny)\n"
        "set_target_properties(main PROPERTIES "
        "RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/tests)\n"
    )
    recipe = Recipe(
        "tiny-c-cmake",
        "tiny",
        "cmake",
        "local-fixture",
        "fixture",
        (),
        False,
        (),
        (),
        ("tiny", "tiny_static", "main"),
        "static",
        Edit("lib/version.c", '"version"', '"version-rllvm-benchmark"'),
    )
    return commit_fixture(
        PreparedFixture(
            "tiny",
            source,
            recipe,
            "fixture",
            "fixture",
            {},
            None,
            tools,
            (),
            root / "prepared fixture.json",
        )
    )


def test_cli_smoke_and_offline_report_use_saved_records(
    tmp_path: Path,
) -> None:
    prepared = _tiny(tmp_path)
    write_json(prepared.manifest_path, prepared.manifest())
    output = tmp_path / "run output with spaces"
    result = runner.invoke(
        app,
        [
            "run",
            "--manifest",
            str(prepared.manifest_path),
            "--smoke",
            "--jobs",
            "2",
            "--extraction-repeats",
            "1",
            "--output",
            str(output),
        ],
    )
    assert result.exit_code == 0, result.stderr
    group = read_json(output / "run-group.json")
    assert group["kind"] == "workflow-run-group"
    runs = cast(list[dict[str, Any]], group["runs"])
    run_manifest = Path(runs[0]["manifest"])
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
