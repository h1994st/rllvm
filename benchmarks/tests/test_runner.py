"""Serial scheduler regressions use real C/C++ builds and capture tools."""

import json
from dataclasses import replace
from pathlib import Path
from typing import Any, cast

import pytest

from benchmarks.fixtures import PreparedFixture
from benchmarks.recipes import Edit, Recipe
from benchmarks.records import read_json
from benchmarks.records import read_records as _read_records
from benchmarks.tests.validation_support import tools_at
from benchmarks.toolchains import Toolchain


def read_records(path: Path) -> list[dict[str, Any]]:
    return cast(list[dict[str, Any]], _read_records(path))


@pytest.fixture
def tiny(tmp_path):
    root = tmp_path / "tiny"
    root.mkdir()
    discovered = tools_at(root)
    tools = Toolchain.discover(
        ("llvm-ranlib", "rllvm-info"),
        root / "extra-tools",
        paths={
            "llvm-ranlib": Path(discovered.path("llvm-ar")).with_name(
                "llvm-ranlib"
            ),
            "rllvm-info": Path(discovered.path("rllvm-cc")).with_name(
                "rllvm-info"
            ),
        },
    )
    tools = replace(discovered, tools=discovered.tools | tools.tools)
    source = root / "source"
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
        "execute_process(COMMAND ${CMAKE_COMMAND} -E environment "
        "OUTPUT_FILE ${CMAKE_BINARY_DIR}/child-environment)\n"
        "file(WRITE ${CMAKE_BINARY_DIR}/child-cwd ${CMAKE_BINARY_DIR})\n"
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
    return PreparedFixture(
        "tiny",
        source,
        recipe,
        "fixture",
        "fixture",
        {},
        None,
        tools,
        (),
        root / "fixture.json",
    )


def test_real_scheduler_preserves_base_references_and_primes_only_base(tiny):
    from benchmarks.runner import RunOptions, run_profile

    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(tiny.source.parent / "run", repetitions=1, jobs=2),
    )
    assert result.valid, read_json(result.root / "run.json")
    assert result.failed_samples == 0
    samples = read_records(result.root / "samples.jsonl")
    assert len(samples) == 12
    assert all(s["valid"] for s in samples)
    commands = read_records(result.root / "commands.jsonl")
    edits = read_records(result.root / "operations.jsonl")
    for arm in (
        "native",
        "wrapped-uncached",
        "wrapped-empty-cache",
        "wrapped-primed-cache",
    ):
        build = result.root / "arms" / arm / "build"
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
        tiny.recipe.edit.after
        not in (tiny.source / tiny.recipe.edit.path).read_text()
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
    diagnostics = read_records(result.root / "diagnostics.jsonl")
    assert diagnostics and all(d["valid"] for d in diagnostics)
    assert diagnostics[1]["summary"]["event_ids"]
    assert set(diagnostics[0]["summary"]["event_ids"]).isdisjoint(
        diagnostics[1]["summary"]["event_ids"]
    )


def test_failed_build_never_extracts_and_keeps_invalid_samples(tiny):
    from benchmarks.runner import RunOptions, run_profile

    (tiny.source / "tests/main.cpp").write_text("not valid C++\n")
    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(
            tiny.source.parent / "run",
            repetitions=1,
            jobs=2,
            diagnostics=False,
        ),
    )
    assert not result.valid
    samples = read_records(result.root / "samples.jsonl")
    assert len(samples) == 12 and not any(s["valid"] for s in samples)
    commands = read_records(result.root / "commands.jsonl")
    assert not any(c["phase"].startswith("extract") for c in commands)
    assert any(c["measurement"]["returncode"] != 0 for c in commands)


def test_stale_library_ir_fails_even_when_both_api_probes_are_edited(tiny):
    from benchmarks.runner import RunOptions, run_profile

    # An actual extractor shim saves original shared IR and serves it after
    # the edit. All native artifacts and API invocations remain real.
    shim = tiny.source.parent / "stale-extractor"
    real = tiny.toolchain.path("rllvm-get-bc")
    shim.write_text(
        "#!/bin/sh\n" + f'{json.dumps(real)} "$@" || exit $?\n'
        'out=""\nprev=""\nfor arg in "$@"; do\n'
        'if [ "$prev" = "-o" ]; then out="$arg"; fi\nprev="$arg"\ndone\n'
        'case "$out" in *shared*)\n'
        f"if /usr/bin/grep -q version-rllvm-benchmark {json.dumps(str(tiny.source / 'lib/version.c'))}; then\n"
        f'cp {json.dumps(str(tiny.source.parent / "old.bc"))} "$out"\n'
        f'else cp "$out" {json.dumps(str(tiny.source.parent / "old.bc"))}; fi;;\nesac\n'
    )
    shim.chmod(0o755)
    tool = replace(tiny.toolchain.tools["rllvm-get-bc"], path=str(shim))
    tools = replace(
        tiny.toolchain,
        tools=tiny.toolchain.tools
        | {
            "rllvm-get-bc": tool,
        },
    )
    result = run_profile(
        tiny,
        tools,
        RunOptions(
            tiny.source.parent / "run",
            repetitions=1,
            jobs=2,
            diagnostics=False,
        ),
    )
    edited = [
        s
        for s in read_records(result.root / "samples.jsonl")
        if s["state"] == "edited" and s["arm"] != "native"
    ]
    assert edited and all(not s["valid"] for s in edited)
    assert all(s["gates"]["api"]["valid"] for s in edited)
    assert all(not s["gates"]["edited_ir"]["valid"] for s in edited)
    assert all(
        "version" in " ".join(s["gates"]["edited_ir"]["failures"])
        for s in edited
    )


def test_missing_independent_evidence_fails_successful_build(tiny):
    from benchmarks.runner import RunOptions, run_profile

    with (tiny.source / "CMakeLists.txt").open("a") as output:
        output.write(
            "add_custom_command(TARGET main POST_BUILD COMMAND "
            "${CMAKE_COMMAND} -E rm -f "
            "${CMAKE_BINARY_DIR}/compile_commands.json)\n"
        )
    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(
            tiny.source.parent / "run",
            repetitions=1,
            jobs=2,
            diagnostics=False,
            extraction_repeats=1,
        ),
    )
    samples = read_records(result.root / "samples.jsonl")
    assert not result.valid
    assert all(s["gates"]["commands"]["valid"] for s in samples)
    assert all(not s["gates"]["coverage:static"]["valid"] for s in samples)


def test_failed_preflight_materializes_all_expected_incomplete_samples(tiny):
    from benchmarks.runner import RunOptions, run_profile

    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(tiny.source.parent / "run", repetitions=1, jobs=0),
    )
    assert result.status == "invalid"
    assert result.failed_samples == 12
    samples = read_records(result.root / "samples.jsonl")
    assert len(samples) == 12 and all(not s["complete"] for s in samples)
    assert "jobs" in " ".join(result.errors)


def test_dry_run_records_the_same_timed_commands_and_resets(tiny):
    from benchmarks.runner import RunOptions, run_profile

    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(
            tiny.source.parent / "run", repetitions=1, jobs=2, dry_run=True
        ),
    )
    assert result.status == "planned"
    assert not result.valid
    commands = read_records(result.root / "commands.jsonl")
    assert commands and all(c["measurement"] is None for c in commands)
    assert len([c for c in commands if c["phase"] == "extract-all"]) == 27
    assert not (result.root / "arms/native/build").exists()
    assert (
        tiny.recipe.edit.before in (tiny.source / "lib/version.c").read_text()
    )
    operations = read_records(result.root / "operations.jsonl")
    assert any(o["operation"] == "restore-source" for o in operations)
    prime = next(c for c in commands if c["phase"] == "prime-build")
    assert prime["command"] == next(
        c["command"]
        for c in commands
        if c["phase"] == "clean-build" and c["arm"] == "wrapped-primed-cache"
    )


def test_interrupt_preserves_failed_command_and_releases_host_lock(tiny):
    import os
    import shutil
    import signal
    import subprocess
    import time

    from benchmarks.workspace import RunLock

    with (tiny.source / "CMakeLists.txt").open("a") as output:
        output.write("execute_process(COMMAND /bin/sleep 30)\n")
    manifest = tiny.source.parent / "prepared.json"
    manifest.write_text(json.dumps(tiny.manifest()))
    script = tiny.source.parent / "run.py"
    root = tiny.source.parent / "interrupted"
    script.write_text(
        "import json\nfrom pathlib import Path\n"
        "from benchmarks.fixtures import PreparedFixture\n"
        "from benchmarks.runner import RunOptions, run_profile\n"
        f"p=PreparedFixture.from_manifest(json.loads(Path({str(manifest)!r}).read_text()))\n"
        f"run_profile(p,p.toolchain,RunOptions(Path({str(root)!r}),repetitions=1,jobs=2))\n"
    )
    uv = shutil.which("uv")
    assert uv
    process = subprocess.Popen(
        (uv, "run", "python", str(script)),
        env=dict(os.environ, PYTHONPATH=str(Path.cwd())),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            commands = root / "operations.jsonl"
            if commands.exists() and any(
                r["operation"] == "command-start"
                for r in read_records(commands)
            ):
                break
            assert process.poll() is None, process.communicate()
            time.sleep(0.05)
        else:
            pytest.fail("runner did not start configuration")
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=15)
        assert process.returncode == 0, (stdout, stderr)
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate()
    result = read_json(root / "run.json")
    assert result["status"] == "interrupted"
    assert result["errors"]
    records = read_records(root / "commands.jsonl")
    assert records[0]["measurement"] is None and records[0]["error"]
    assert len(read_records(root / "samples.jsonl")) == 12
    with RunLock():
        pass


class StaticCargoRecipe(Recipe):
    """A tiny static Cargo fixture with a real linked C API consumer."""

    def targets(self, host):
        return tuple(t for t in super().targets(host) if t.kind != "shared")

    def version_probe_source(self):
        return (
            "#include <stdio.h>\nextern const char *quiche_version(void);\n"
            "int main(void){puts(quiche_version());return 0;}\n"
        )

    def version_probe_commands(
        self, source, build, probe_source, output, tools, *, env
    ):
        from benchmarks.process import Command

        return (
            Command(
                (
                    tools.path("clang"),
                    str(probe_source),
                    str(build / "release/libquiche.a"),
                    *(
                        ("-liconv",)
                        if tools.host == "darwin"
                        else ("-ldl", "-lpthread", "-lm")
                    ),
                    "-o",
                    str(output),
                ),
                build,
                dict(env),
            ),
            Command((str(output),), build, dict(env)),
        )


def test_real_cargo_scheduler_keeps_artifact_and_bitcode_cache_states(tiny):
    from benchmarks.runner import RunOptions, run_profile

    source = tiny.source
    (source / "Cargo.toml").write_text(
        '[workspace]\nmembers=["quiche"]\nresolver="2"\n'
    )
    crate = source / "quiche"
    (crate / "src").mkdir(parents=True)
    (crate / "include").mkdir()
    (crate / "examples").mkdir()
    (crate / "Cargo.toml").write_text(
        '[package]\nname="quiche"\nversion="0.1.0"\nedition="2024"\n'
        '[lib]\ncrate-type=["rlib","staticlib","cdylib"]\n'
        "[features]\nffi=[]\n"
    )
    (source / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname="quiche"\nversion="0.1.0"\n'
    )
    (crate / "include/quiche.h").write_text(
        "const char *quiche_version(void);\n"
    )
    (crate / "src/lib.rs").write_text(
        '#[unsafe(no_mangle)]\npub extern "C" fn quiche_version() -> *const u8 {\n'
        'b"version\\0".as_ptr()\n}\n'
    )
    (crate / "examples/client.rs").write_text(
        'fn main(){unsafe{println!("{}",std::ffi::CStr::from_ptr('
        "quiche::quiche_version().cast()).to_str().unwrap());}}\n"
    )
    prepared = replace(
        tiny,
        recipe=replace(
            StaticCargoRecipe(**tiny.recipe.__dict__),
            profile_id="tiny-cargo",
            project="quiche",
            build_system="cargo",
            build_targets=("quiche", "client"),
            edit=Edit(
                "quiche/src/lib.rs",
                'b"version\\0"',
                'b"version-rllvm-benchmark\\0"',
            ),
        ),
    )
    result = run_profile(
        prepared,
        tiny.toolchain,
        RunOptions(
            source.parent / "run", repetitions=1, jobs=2, extraction_repeats=1
        ),
    )
    assert result.valid, read_json(result.root / "run.json")
    samples = read_records(result.root / "samples.jsonl")
    assert len(samples) == 12 and all(s["valid"] for s in samples)
    assert all(
        s["cargo_artifact_state"] == "retained"
        for s in samples
        if s["state"] == "edited"
    )


def test_diagnostic_recording_failure_invalidates_every_sample(tiny):
    from benchmarks.runner import RunOptions, run_profile

    with (tiny.source / "CMakeLists.txt").open("a") as output:
        output.write(
            "if(DEFINED ENV{RLLVM_BENCHMARK_EVENTS})\n"
            'file(WRITE "$ENV{RLLVM_BENCHMARK_EVENTS}/../clang/health-seal" "{")\n'
            "endif()\n"
        )
    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(
            tiny.source.parent / "run",
            repetitions=1,
            jobs=2,
            extraction_repeats=1,
        ),
    )
    assert not result.valid and result.failed_samples == 12
    samples = read_records(result.root / "samples.jsonl")
    assert all(s["gates"]["api"]["valid"] for s in samples)
    assert all(not s["gates"]["diagnostics"]["valid"] for s in samples)
    replay = read_records(result.root / "diagnostics.jsonl")
    assert len(replay) == 4 and all(not r["valid"] for r in replay)
    assert (
        result.root / "diagnostic/000/clang/health-seal"
    ).read_text() == "{"


def test_repetitions_balance_positions_and_reuse_stable_command_paths(tiny):
    from benchmarks.runner import RunOptions, run_profile

    result = run_profile(
        tiny,
        tiny.toolchain,
        RunOptions(
            tiny.source.parent / "run", repetitions=4, jobs=2, dry_run=True
        ),
    )
    commands = read_records(result.root / "commands.jsonl")
    builds = [r for r in commands if r["phase"] == "clean-build"]
    for position in range(4):
        assert (
            len({builds[rep * 4 + position]["arm"] for rep in range(4)}) == 4
        )
    for arm in {r["arm"] for r in builds}:
        observations = [r["command"] for r in builds if r["arm"] == arm]
        assert observations == [observations[0]] * 4
    assert len(read_records(result.root / "samples.jsonl")) == 48
