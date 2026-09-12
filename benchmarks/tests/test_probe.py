"""Native subprocess boundaries for separate diagnostic probes."""

import concurrent.futures
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from benchmarks.probe import create_probe, read_events, summarize_events
from benchmarks.tests.validation_support import environment, run, tools_at


class ProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="probe space ")
        cls.root = Path(cls.temporary.name)
        cls.tools = tools_at(cls.root)
        cls.env = environment(cls.root, cls.tools)
        source = cls.root / "echo.c"
        source.write_text(
            "#include <stdio.h>\n#include <string.h>\n"
            "int main(int n,char **v){for(int i=0;i<n;i++){"
            "fwrite(v[i],1,strlen(v[i]),stdout);putchar(0);}return 37;}\n"
        )
        cls.child = cls.root / "echo"
        run(
            (cls.tools.path("clang"), source, "-o", cls.child),
            cls.root,
            cls.env,
            "echo-build",
        )

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

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
        self.assertEqual(result.returncode, 37)
        self.assertEqual(result.stdout, b"\0".join(argv) + b"\0")
        events = read_events(probe.events)
        self.assertEqual(len(events), 1)
        self.assertEqual([os.fsencode(s) for s in events[0].argv], argv)

    def test_concurrent_probes_have_distinct_complete_events(self):
        probe = self.probe()

        def invoke(i):
            return subprocess.run(
                (probe.path, str(i)), env=self.env, capture_output=True
            ).returncode

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            self.assertEqual(list(pool.map(invoke, range(12))), [37] * 12)
        events = read_events(probe.events)
        self.assertEqual(len(events), 12)
        self.assertEqual(len({e.event_id for e in events}), 12)

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
        self.assertEqual(
            subprocess.check_output((self.root / "alias",)), b"42"
        )
        rust = self.probe(Path(self.tools.path("rustc")), "rustc", "rustc")
        output = subprocess.check_output((rust.path, "-vV"), env=self.env)
        self.assertIn(b"LLVM version:", output)

    def test_response_flags_are_unknown_not_false_compile_zeros(self):
        probe = self.probe()
        subprocess.run(
            (probe.path, "@hidden.rsp"), env=self.env, capture_output=True
        )
        summary = summarize_events(read_events(probe.events))
        self.assertEqual(summary.counts["compiler-driver"], 1)
        self.assertIsNone(summary.preprocess.count)
        self.assertTrue(summary.preprocess.reason)
        self.assertIsNone(summary.cache_hits.count)

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
            events, measurements=(warm,), cache_trace=True
        )
        self.assertTrue(summary.valid, summary.failures)
        self.assertEqual(summary.cache_hits.count, 1)
        self.assertEqual(summary.preprocess.count, 1)
        self.assertEqual(summary.bitcode_compilations.count, 0)
        self.assertEqual(
            session.environment["RLLVM_BENCHMARK_EVENTS"], str(session.events)
        )
        self.assertFalse((self.root / "timed-cache").exists())
        failed = summarize_events(events, measurements=(replace_status(cold),))
        self.assertFalse(failed.valid)

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
        self.assertEqual(summary.counts, {"llvm-link": 1, "llvm-ar": 1})
        self.assertTrue(summary.unobserved["rustc"])


def replace_status(measurement):
    from dataclasses import replace

    return replace(measurement, returncode=1)


class ProbeRecordTests(unittest.TestCase):
    def test_parent_record_round_trips_filesystem_surrogates(self):
        from benchmarks.records import (
            append_record,
            read_json,
            read_records,
            write_json,
        )

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "record.json"
            value = {
                "schema_version": 1,
                "argv": [os.fsdecode(b"arg-\xff"), "字", ""],
            }
            write_json(path, value)
            self.assertEqual(read_json(path), value)
            append_record(path.with_suffix(".jsonl"), value)
            self.assertEqual(read_records(path.with_suffix(".jsonl")), [value])
