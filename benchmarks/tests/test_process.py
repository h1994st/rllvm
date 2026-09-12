import errno
import os
import signal
import sys
import time
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from benchmarks.process import Command, CommandFailed, checked, execute
from benchmarks.workspace import RunLock


def process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class ProcessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.logs = self.root / "logs"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def command(self, *argv: str) -> Command:
        return Command(argv, self.root, {"PATH": os.defpath})

    def test_failed_command_retains_stderr_status_and_usage(self) -> None:
        command = self.command("sh", "-c", "printf failure >&2; exit 7")

        result = execute(command, self.logs, "failure")

        self.assertEqual(result.returncode, 7)
        self.assertEqual(Path(result.stderr).read_text(), "failure")
        assert result.wall_seconds is not None
        assert result.user_cpu_seconds is not None
        assert result.system_cpu_seconds is not None
        assert result.max_process_rss_bytes is not None
        self.assertGreaterEqual(result.wall_seconds, 0)
        self.assertGreaterEqual(result.user_cpu_seconds, 0)
        self.assertGreaterEqual(result.system_cpu_seconds, 0)
        self.assertGreaterEqual(result.max_process_rss_bytes, 0)
        self.assertIn("direct-child", result.resource_method)
        self.assertIsNone(result.failure)

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

        self.assertEqual(result.returncode, 0)
        assert result.user_cpu_seconds is not None
        self.assertGreater(result.user_cpu_seconds, 0.05)

    def test_missing_executable_is_a_structured_spawn_failure(self) -> None:
        command = self.command(
            "/definitely/not/a/real/rllvm-benchmark-command"
        )

        result = execute(command, self.logs, "missing")

        self.assertIsNone(result.returncode)
        self.assertIsNone(result.wall_seconds)
        self.assertIsNone(result.user_cpu_seconds)
        self.assertIsNone(result.system_cpu_seconds)
        self.assertIsNone(result.max_process_rss_bytes)
        self.assertEqual(result.resource_method, "not-available:spawn-failed")
        self.assertIsNotNone(result.failure)
        assert result.failure is not None
        self.assertEqual(result.failure.kind, "spawn")
        self.assertEqual(result.failure.errno, errno.ENOENT)
        self.assertEqual(Path(result.stdout).read_text(), "")
        self.assertEqual(Path(result.stderr).read_text(), "")

    def test_checked_raises_with_the_failed_measurement(self) -> None:
        command = self.command("sh", "-c", "exit 23")

        with self.assertRaises(CommandFailed) as raised:
            checked(command, self.logs, "checked")

        self.assertEqual(raised.exception.result.returncode, 23)

    def test_signal_exit_retains_negative_signal_returncode(self) -> None:
        command = self.command("sh", "-c", "kill -TERM $$")

        result = execute(command, self.logs, "signal")

        self.assertEqual(result.returncode, -signal.SIGTERM)

    def test_interruption_reaps_the_process_group_and_releases_lock(
        self,
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
                        self.fail("child process did not start")
                    time.sleep(0.01)
                interrupted = True
                raise KeyboardInterrupt
            return real_wait4(pid, options)

        with self.assertRaises(KeyboardInterrupt):
            with RunLock(lock_path):
                with patch(
                    "benchmarks.process.os.wait4",
                    side_effect=interrupt_after_child_started,
                ):
                    execute(
                        self.command(sys.executable, "-c", script),
                        self.logs,
                        "interrupted",
                    )

        child_pid = int(child_pid_path.read_text())
        deadline = time.monotonic() + 3
        while process_exists(child_pid) and time.monotonic() < deadline:
            time.sleep(0.01)
        try:
            self.assertFalse(process_exists(child_pid))
        finally:
            if process_exists(child_pid):
                os.kill(child_pid, signal.SIGKILL)
        self.assertTrue((self.logs / "interrupted.stdout.log").exists())
        self.assertTrue((self.logs / "interrupted.stderr.log").exists())
        self.assertEqual(
            (self.logs / "interrupted.stdout.log").read_text(),
            "started\n",
        )
        with RunLock(lock_path):
            pass


if __name__ == "__main__":
    unittest.main()
