import errno
import json
import os
import resource
import signal
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from benchmarks.process import Command, CommandFailed, checked, execute
from benchmarks.workspace import RunLock


def process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class TestProcess:
    @pytest.fixture(autouse=True)
    def _process_root(self, tmp_path: Path) -> None:
        self.root = tmp_path
        self.logs = self.root / "logs"

    def command(self, *argv: str) -> Command:
        return Command(argv, self.root, {"PATH": os.defpath})

    def test_failed_command_retains_stderr_status_and_usage(self) -> None:
        command = self.command("sh", "-c", "printf failure >&2; exit 7")

        result = execute(command, self.logs, "failure")

        assert result.returncode == 7
        assert Path(result.stderr).read_text() == "failure"
        assert result.wall_seconds is not None
        assert result.user_cpu_seconds is not None
        assert result.system_cpu_seconds is not None
        assert result.max_process_rss_bytes is not None
        assert result.wall_seconds >= 0
        assert result.user_cpu_seconds >= 0
        assert result.system_cpu_seconds >= 0
        assert result.max_process_rss_bytes >= 0
        assert "waited-command" in result.resource_method
        assert result.failure is None

    def test_cpu_consuming_child_has_per_process_cpu_usage(self) -> None:
        script = (
            "import time\n"
            "start = time.process_time()\n"
            "while time.process_time() - start < 0.15:\n"
            "    pass\n"
        )

        result = execute(
            self.command(sys.executable, "-c", script),
            self.logs,
            "cpu",
        )

        assert result.returncode == 0
        assert result.user_cpu_seconds is not None
        assert result.system_cpu_seconds is not None
        # process_time() measures user + system CPU, with no fixed split.
        assert result.user_cpu_seconds + result.system_cpu_seconds > 0.05

    def test_wait4_usage_includes_waited_descendants(self) -> None:
        child_script = (
            "import time\n"
            "data = bytearray(64 * 1024 * 1024)\n"
            "for offset in range(0, len(data), 4096):\n"
            "    data[offset] = 1\n"
            "start = time.process_time()\n"
            "while time.process_time() - start < 0.15:\n"
            "    pass\n"
        )
        parent_script = (
            "import json, resource, subprocess, sys\n"
            f"subprocess.run([sys.executable, '-c', {child_script!r}], "
            "check=True)\n"
            "usage = resource.getrusage(resource.RUSAGE_SELF)\n"
            "descendants = resource.getrusage(resource.RUSAGE_CHILDREN)\n"
            "print(json.dumps({'cpu_seconds': "
            "usage.ru_utime + usage.ru_stime + "
            "descendants.ru_utime + descendants.ru_stime, "
            "'max_rss': max(usage.ru_maxrss, descendants.ru_maxrss), "
            "'descendant_max_rss': descendants.ru_maxrss}))\n"
        )

        result = execute(
            self.command(sys.executable, "-c", parent_script),
            self.logs,
            "descendants",
        )

        assert result.returncode == 0
        observed_usage = json.loads(Path(result.stdout).read_text())
        assert result.user_cpu_seconds is not None
        assert result.system_cpu_seconds is not None
        assert result.max_process_rss_bytes is not None
        rss_unit = 1 if sys.platform == "darwin" else 1024
        assert result.user_cpu_seconds + result.system_cpu_seconds >= float(
            observed_usage["cpu_seconds"]
        )
        assert (
            observed_usage["descendant_max_rss"] * rss_unit >= 64 * 1024 * 1024
        )
        # Linux preserves the pre-exec RSS peak, which can come from pytest.
        # The parent's high-water may exceed even the allocating descendant's.
        assert (
            result.max_process_rss_bytes
            >= observed_usage["max_rss"] * rss_unit
        )
        assert (
            "waited-command-and-reaped-descendants" in result.resource_method
        )
        assert "not-simultaneous-tree-peak" in result.resource_method

    def test_missing_executable_is_a_structured_spawn_failure(self) -> None:
        command = self.command(
            "/definitely/not/a/real/rllvm-benchmark-command"
        )

        result = execute(command, self.logs, "missing")

        assert result.returncode is None
        assert result.wall_seconds is None
        assert result.user_cpu_seconds is None
        assert result.system_cpu_seconds is None
        assert result.max_process_rss_bytes is None
        assert result.resource_method == "not-available:spawn-failed"
        assert result.failure is not None
        assert result.failure.kind == "spawn"
        assert result.failure.errno == errno.ENOENT
        assert Path(result.stdout).read_text() == ""
        assert Path(result.stderr).read_text() == ""

    def test_checked_raises_with_the_failed_measurement(self) -> None:
        command = self.command("sh", "-c", "exit 23")

        with pytest.raises(CommandFailed) as raised:
            checked(command, self.logs, "checked")

        assert raised.value.result.returncode == 23

    @pytest.mark.parametrize("stream", ["stdout", "stderr"])
    def test_symlink_log_collision_preserves_unrelated_target(
        self, stream: str
    ) -> None:
        self.logs.mkdir()
        label = f"collision-{stream}"
        sentinel = self.root / f"unrelated-{stream}.log"
        sentinel.write_text("preserve original")
        collision = self.logs / f"{label}.{stream}.log"
        collision.symlink_to(sentinel)

        with pytest.raises(FileExistsError):
            execute(
                self.command("sh", "-c", "printf replacement"),
                self.logs,
                label,
            )

        other_stream = "stderr" if stream == "stdout" else "stdout"
        other_log = self.logs / f"{label}.{other_stream}.log"
        assert collision.is_symlink()
        assert sentinel.read_text() == "preserve original"
        assert not other_log.exists()

    def test_reused_label_preserves_prior_logs(self) -> None:
        first = execute(
            self.command(
                "sh",
                "-c",
                "printf first-output; printf first-error >&2",
            ),
            self.logs,
            "collision",
        )
        stdout_before = Path(first.stdout).read_bytes()
        stderr_before = Path(first.stderr).read_bytes()

        with pytest.raises(FileExistsError):
            execute(
                self.command(
                    "sh",
                    "-c",
                    "printf replacement; printf replaced-error >&2",
                ),
                self.logs,
                "collision",
            )

        assert Path(first.stdout).read_bytes() == stdout_before
        assert Path(first.stderr).read_bytes() == stderr_before

    def test_signal_exit_retains_negative_signal_returncode(self) -> None:
        command = self.command("sh", "-c", "kill -TERM $$")

        result = execute(command, self.logs, "signal")

        assert result.returncode == -signal.SIGTERM

    def test_interruption_reaps_the_process_group_and_releases_lock(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        child_pid_path = self.root / "child.pid"
        lock_path = self.root / "run.lock"
        grandchild_script = (
            "import os, pathlib, signal, time\n"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
            f"pathlib.Path({str(child_pid_path)!r}).write_text(str(os.getpid()))\n"
            "time.sleep(60)\n"
        )
        script = (
            "import subprocess, sys, time\n"
            "print('started', flush=True)\n"
            f"subprocess.Popen([sys.executable, '-c', {grandchild_script!r}])\n"
            "time.sleep(60)\n"
        )
        real_wait4 = os.wait4
        interrupted = False

        def interrupt_after_child_started(
            pid: int, options: int
        ) -> tuple[int, int, object]:
            nonlocal interrupted
            if not interrupted:
                deadline = time.monotonic() + 3
                while not child_pid_path.exists():
                    if time.monotonic() >= deadline:
                        pytest.fail("child process did not start")
                    time.sleep(0.01)
                interrupted = True
                raise KeyboardInterrupt
            return real_wait4(pid, options)

        def run_interrupted_command() -> None:
            with RunLock(lock_path):
                execute(
                    self.command(sys.executable, "-c", script),
                    self.logs,
                    "interrupted",
                )

        monkeypatch.setattr(
            "benchmarks.process.os.wait4", interrupt_after_child_started
        )
        with pytest.raises(KeyboardInterrupt):
            run_interrupted_command()

        child_pid = int(child_pid_path.read_text())
        deadline = time.monotonic() + 3
        while process_exists(child_pid) and time.monotonic() < deadline:
            time.sleep(0.01)
        try:
            assert not process_exists(child_pid)
        finally:
            if process_exists(child_pid):
                os.kill(child_pid, signal.SIGKILL)
        assert (self.logs / "interrupted.stdout.log").exists()
        assert (self.logs / "interrupted.stderr.log").exists()
        assert (
            self.logs / "interrupted.stdout.log"
        ).read_text() == "started\n"
        with RunLock(lock_path):
            pass


@pytest.mark.parametrize(
    ("platform", "expected_rss_bytes"),
    [("darwin", 12345), ("linux", 12641280)],
)
def test_wait4_cpu_fields_and_rss_units_are_preserved(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    platform: str,
    expected_rss_bytes: int,
) -> None:
    process = SimpleNamespace(pid=123, returncode=None)
    usage = resource.struct_rusage((1.25, 2.5, 12345, *([0] * 13)))
    monkeypatch.setattr(
        "benchmarks.process.subprocess.Popen", lambda *args, **kwargs: process
    )
    monkeypatch.setattr(
        "benchmarks.process.os.wait4", lambda pid, options: (pid, 0, usage)
    )
    monkeypatch.setattr(
        "benchmarks.process.sys", SimpleNamespace(platform=platform)
    )

    result = execute(Command(("command",), tmp_path, {}), tmp_path, "usage")

    assert result.returncode == 0
    assert result.user_cpu_seconds == 1.25
    assert result.system_cpu_seconds == 2.5
    assert result.max_process_rss_bytes == expected_rss_bytes
