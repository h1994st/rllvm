"""Reciprocal health-channel failure and duplicate-observation regressions."""

import os
from pathlib import Path

import pytest

from benchmarks.probe import create_probe, read_events, summarize_events
from benchmarks.process import Command, execute
from benchmarks.tests.validation_support import environment, run, tools_at

pytestmark = pytest.mark.full


@pytest.fixture
def probe_context(tmp_path: Path):
    tools = tools_at(tmp_path)
    env = environment(tmp_path, tools)
    source, child = tmp_path / "child.c", tmp_path / "child"
    source.write_text(
        "#include <stdio.h>\n#include <string.h>\n"
        "int main(int n,char **v){for(int i=0;i<n;i++){"
        "fwrite(v[i],1,strlen(v[i]),stdout);putchar(0);}return 37;}\n"
    )
    run((tools.path("clang"), source, "-o", child), tmp_path, env, "compile")
    return tmp_path, tools, env, child


@pytest.mark.parametrize("failure", ("read-only", "directory"))
def test_failed_seal_preserves_child_when_receipt_storage_works(
    probe_context, failure: str
):
    if failure == "read-only" and os.geteuid() == 0:
        pytest.skip("root bypasses ordinary read-only file permissions")
    root, tools, env, child = probe_context
    probe = create_probe(
        root / "probe", child, "clang", "compiler-driver", tools, env=env
    )
    seal = Path(probe.health.seal)
    if failure == "read-only":
        seal.chmod(0o400)
    else:
        seal.unlink()
        seal.mkdir()
    argv = (
        str(probe.path),
        "space argument",
        "",
        os.fsdecode(b"nonutf8-\xff"),
    )
    result = execute(Command(argv, root, env), root / "logs", "child")
    assert result.returncode == 37
    assert (
        Path(result.stdout).read_bytes()
        == b"\0".join(map(os.fsencode, argv)) + b"\0"
    )
    receipts = tuple(Path(probe.health.directory).glob("attempt-*"))
    assert len(receipts) == 1
    assert receipts[0].read_bytes() == b""
    summary = summarize_events(
        read_events(probe.events),
        measurements=(result,),
        health=(probe.health,),
    )
    assert not summary.valid
    assert any("health" in failure for failure in summary.failures)


def test_duplicate_event_ids_cannot_authorize_doubled_counts(probe_context):
    root, tools, env, _ = probe_context
    probe = create_probe(
        root / "probe",
        Path("/usr/bin/true"),
        "clang",
        "compiler-driver",
        tools,
        env=env,
    )
    result = run((probe.path,), root, env, "child")
    events = read_events(probe.events)
    assert len(events) == 1
    assert summarize_events(
        events, measurements=(result,), health=(probe.health,)
    ).valid
    duplicate = summarize_events(
        events + events, measurements=(result,), health=(probe.health,)
    )
    assert not duplicate.valid
    assert any("duplicate" in failure for failure in duplicate.failures)


def test_child_is_not_run_when_neither_health_channel_can_record(
    probe_context,
):
    root, tools, env, child = probe_context
    probe = create_probe(
        root / "probe", child, "clang", "compiler-driver", tools, env=env
    )
    Path(probe.health.seal).unlink()
    Path(probe.health.directory).rmdir()
    result = execute(
        Command((str(probe.path),), root, env), root / "logs", "child"
    )
    assert result.returncode == 126
    assert Path(result.stdout).read_bytes() == b""
    assert not summarize_events(
        read_events(probe.events),
        measurements=(result,),
        health=(probe.health,),
    ).valid
