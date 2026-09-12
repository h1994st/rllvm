"""Native subprocess boundaries for separate diagnostic probes."""

import concurrent.futures
import os
import subprocess
import tempfile
from pathlib import Path

import pytest

from benchmarks.probe import create_probe, read_events, summarize_events
from benchmarks.tests.validation_support import environment, run, tools_at
from benchmarks.toolchains import Toolchain


@pytest.fixture
def _probe_context(request, tmp_path: Path) -> None:
    root = tmp_path / "probe space"
    root.mkdir()
    tools = tools_at(root)
    env = environment(root, tools)
    source = root / "echo.c"
    source.write_text(
        "#include <stdio.h>\n#include <string.h>\n"
        "int main(int n,char **v){for(int i=0;i<n;i++){"
        "fwrite(v[i],1,strlen(v[i]),stdout);putchar(0);}return 37;}\n"
    )
    child = root / "echo"
    run((tools.path("clang"), source, "-o", child), root, env, "echo-build")
    instance = request.instance
    instance.root = root
    instance.tools = tools
    instance.env = env
    instance.child = child


@pytest.mark.full
@pytest.mark.usefixtures("_probe_context")
class TestProbe:
    root: Path
    tools: Toolchain
    env: dict[str, str]
    child: Path

    def probe(self, real=None, name="clang", kind="compiler-driver"):
        root = Path(tempfile.mkdtemp(dir=self.root))
        return create_probe(
            root, real or self.child, name, kind, self.tools, env=self.env
        )

    def test_preserves_bytes_empty_custom_argv0_and_exit(self):
        probe = self.probe()
        argv = [b"custom-clang++", b"space argument", b"", b"nonutf8-\xff"]
        result = subprocess.run(
            argv, executable=probe.path, env=self.env, capture_output=True
        )
        assert result.returncode == 37
        assert result.stdout == b"\x00".join(argv) + b"\x00"
        events = read_events(probe.events)
        assert len(events) == 1
        assert [os.fsencode(s) for s in events[0].argv] == argv

    def test_concurrent_probes_have_distinct_complete_events(self):
        probe = self.probe()

        def invoke(i):
            return subprocess.run(
                (probe.path, str(i)), env=self.env, capture_output=True
            ).returncode

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            assert list(pool.map(invoke, range(12))) == [37] * 12
        events = read_events(probe.events)
        assert len(events) == 12
        assert len({e.event_id for e in events}) == 12

    def test_cxx_and_rustup_aliases_keep_driver_behavior(self):
        probe = self.probe(Path(self.tools.path("clang++")), "clang++")
        source = self.root / "alias.cpp"
        source.write_text("#include <iostream>\nint main(){std::cout<<42;}")
        run(
            (probe.path, source, "-o", self.root / "alias"),
            self.root,
            self.env,
            "alias",
        )
        assert subprocess.check_output((self.root / "alias",)) == b"42"
        rust = self.probe(Path(self.tools.path("rustc")), "rustc", "rustc")
        output = subprocess.check_output((rust.path, "-vV"), env=self.env)
        assert b"LLVM version:" in output

    def test_response_flags_are_unknown_not_false_compile_zeros(self):
        probe = self.probe()
        subprocess.run(
            (probe.path, "@hidden.rsp"), env=self.env, capture_output=True
        )
        summary = summarize_events(read_events(probe.events))
        assert summary.counts["compiler-driver"] == 1
        assert summary.preprocess.count is None
        assert summary.preprocess.reason
        assert summary.cache_hits.count is None

    def test_diagnostic_cache_replay_has_stable_environment_and_real_hits(
        self,
    ):
        from benchmarks.probe import prepare_diagnostics

        root = self.root / "diagnostic-session"
        session = prepare_diagnostics(
            root, self.root / "timed-cache", self.tools, env=self.env
        )
        source = root / "value.c"
        source.write_text("int project_value(void){return 42;}")
        command = (
            self.tools.path("rllvm-cc"),
            "--rllvm-verbose=3",
            "-c",
            source,
            "-o",
            root / "value.o",
        )
        cold = run(command, root, session.environment, "cold")
        previous = {e.event_id for e in read_events(session.events)}
        warm = run(command, root, session.environment, "warm")
        events = tuple(
            e
            for e in read_events(session.events)
            if e.event_id not in previous
        )
        summary = summarize_events(
            events,
            measurements=(warm,),
            cache_trace=True,
            health=session.health,
            prior_event_ids=frozenset(previous),
        )
        assert summary.valid, summary.failures
        assert summary.cache_hits.count == 1
        assert summary.preprocess.count == 1
        assert summary.bitcode_compilations.count == 0
        assert session.environment["RLLVM_BENCHMARK_EVENTS"] == str(
            session.events
        )
        assert not (self.root / "timed-cache").exists()
        failed = summarize_events(events, measurements=(replace_status(cold),))
        assert not failed.valid

    def test_linker_and_archiver_counts_are_observed_execs(self):
        link = self.probe(
            Path(self.tools.path("llvm-link")), "llvm-link", "llvm-link"
        )
        archive = self.probe(
            Path(self.tools.path("llvm-ar")), "llvm-ar", "llvm-ar"
        )
        run((link.path, "--version"), self.root, self.env, "link-version")
        run((archive.path, "--version"), self.root, self.env, "ar-version")
        events = read_events(link.events) + read_events(archive.events)
        summary = summarize_events(events)
        assert summary.counts == {"llvm-link": 1, "llvm-ar": 1}
        assert summary.unobserved["rustc"]

    def test_hidden_event_failure_invalidates_successful_parent(self):
        import sys

        probe = self.probe(Path("/usr/bin/true"))
        parent = run(
            (
                sys.executable,
                "-c",
                """
import os, subprocess, sys
bad = dict(os.environ, RLLVM_BENCHMARK_EVENTS=sys.argv[2])
for env in (bad, os.environ):
    child = subprocess.run([sys.argv[1]], env=env, capture_output=True)
    assert child.returncode == 0
""",
                probe.path,
                probe.events / "missing",
            ),
            self.root,
            self.env,
            "hidden-error",
        )
        assert Path(parent.stderr).read_bytes() == b""
        events = read_events(probe.events)
        assert len(events) == 1
        summary = summarize_events(
            events, measurements=(parent,), health=(probe.health,)
        )
        assert not summary.valid, summary

    def test_health_receipts_are_required_complete_and_cover_the_event_slice(
        self,
    ):
        probe = self.probe(Path("/usr/bin/true"))
        parent = run((probe.path,), self.root, self.env, "health-valid")
        events = read_events(probe.events)
        good = summarize_events(
            events, measurements=(parent,), health=(probe.health,)
        )
        assert good.valid, good.failures
        assert not summarize_events(events, measurements=(parent,)).valid
        assert not summarize_events(
            (), measurements=(parent,), health=(probe.health,)
        ).valid
        pending = Path(probe.health.directory) / "attempt-interrupted"
        pending.touch()
        assert not summarize_events(
            events, measurements=(parent,), health=(probe.health,)
        ).valid

    def test_unavailable_receipt_directory_poisons_independent_seal(self):
        probe = self.probe(Path("/usr/bin/true"))
        directory = Path(probe.health.directory)
        parked = directory.with_name("parked-health")
        directory.rename(parked)
        parent = run((probe.path,), self.root, self.env, "health-unavailable")
        parked.rename(directory)
        # Restore the directory and collect another successful event: the lost
        # invocation must remain visible even though its child returned zero.
        later = run((probe.path,), self.root, self.env, "health-later")
        events = read_events(probe.events)
        assert len(events) == 1
        result = summarize_events(
            events, measurements=(parent, later), health=(probe.health,)
        )
        assert not result.valid
        assert "health seal" in " ".join(result.failures)


def replace_status(measurement):
    from dataclasses import replace

    return replace(measurement, returncode=1)


class TestProbeRecords:
    def test_parent_record_round_trips_filesystem_surrogates(
        self, tmp_path: Path
    ):
        from benchmarks.records import (
            append_record,
            read_json,
            read_records,
            write_json,
        )

        path = tmp_path / "record.json"
        value = {
            "schema_version": 1,
            "argv": [os.fsdecode(b"arg-\xff"), "字", ""],
        }
        write_json(path, value)
        assert read_json(path) == value
        append_record(path.with_suffix(".jsonl"), value)
        assert read_records(path.with_suffix(".jsonl")) == [value]
