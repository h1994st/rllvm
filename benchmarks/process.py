"""Execute benchmark commands and retain their measurements."""

import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, BinaryIO


@dataclass(frozen=True)
class Command:
    argv: tuple[str, ...]
    cwd: Path
    env: dict[str, str]


@dataclass(frozen=True)
class SpawnFailure:
    """A failure that happened before a command acquired a process ID."""

    kind: str
    exception_type: str
    message: str
    errno: int | None


@dataclass(frozen=True)
class Measurement:
    argv: tuple[str, ...]
    cwd: str
    returncode: int | None
    started_utc: str
    wall_seconds: float | None
    user_cpu_seconds: float | None
    system_cpu_seconds: float | None
    max_process_rss_bytes: int | None
    stdout: str
    stderr: str
    resource_method: str
    failure: SpawnFailure | None = None


class CommandFailed(RuntimeError):
    """A command completed unsuccessfully or could not be started."""

    def __init__(self, result: Measurement) -> None:
        self.result = result
        if result.failure is not None:
            detail = result.failure.message
        else:
            detail = f"exit status {result.returncode}"
        super().__init__(f"command failed ({detail}): {result.argv!r}")


def execute(command: Command, logs: Path, label: str) -> Measurement:
    """Run a command and return its status and descendant-aware wait4 usage."""
    _validate_label(label)
    logs.mkdir(parents=True, exist_ok=True)
    stdout_path = logs / f"{label}.stdout.log"
    stderr_path = logs / f"{label}.stderr.log"
    started_utc = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    started = time.monotonic()

    stdout, stderr = _open_logs_exclusively(stdout_path, stderr_path)
    with stdout, stderr:
        try:
            process = subprocess.Popen(
                command.argv,
                cwd=command.cwd,
                env=command.env,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
            )
        except Exception as error:
            return Measurement(
                argv=command.argv,
                cwd=str(command.cwd),
                returncode=None,
                started_utc=started_utc,
                wall_seconds=None,
                user_cpu_seconds=None,
                system_cpu_seconds=None,
                max_process_rss_bytes=None,
                stdout=str(stdout_path),
                stderr=str(stderr_path),
                resource_method="not-available:spawn-failed",
                failure=SpawnFailure(
                    kind="spawn",
                    exception_type=type(error).__name__,
                    message=str(error),
                    errno=getattr(error, "errno", None),
                ),
            )

        try:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
        except BaseException:
            _terminate_and_reap(process)
            raise

    return Measurement(
        argv=command.argv,
        cwd=str(command.cwd),
        returncode=process.returncode,
        started_utc=started_utc,
        wall_seconds=time.monotonic() - started,
        user_cpu_seconds=float(usage.ru_utime),
        system_cpu_seconds=float(usage.ru_stime),
        max_process_rss_bytes=_rss_bytes(usage.ru_maxrss),
        stdout=str(stdout_path),
        stderr=str(stderr_path),
        resource_method=_resource_method(),
    )


def checked(command: Command, logs: Path, label: str) -> Measurement:
    """Run a command and raise with its measurement if it did not succeed."""
    result = execute(command, logs, label)
    if result.returncode != 0:
        raise CommandFailed(result)
    return result


def _validate_label(label: str) -> None:
    if not label or Path(label).name != label or label in {".", ".."}:
        raise ValueError("log label must be a nonempty filename component")


def _open_logs_exclusively(
    stdout_path: Path, stderr_path: Path
) -> tuple[BinaryIO, BinaryIO]:
    stdout = stdout_path.open("xb")
    try:
        stderr = stderr_path.open("xb")
    except BaseException:
        stdout.close()
        stdout_path.unlink()
        raise
    return stdout, stderr


def _terminate_and_reap(process: subprocess.Popen[Any]) -> None:
    process_group = process.pid
    try:
        os.killpg(process_group, signal.SIGTERM)
    except PermissionError, ProcessLookupError:
        pass

    deadline = time.monotonic() + 1
    leader_reaped = False
    while time.monotonic() < deadline:
        if not leader_reaped:
            try:
                waited_pid, status, _ = os.wait4(process.pid, os.WNOHANG)
            except ChildProcessError:
                leader_reaped = True
            else:
                if waited_pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    leader_reaped = True
        if leader_reaped and not _process_group_exists(process_group):
            return
        time.sleep(0.01)

    try:
        os.killpg(process_group, signal.SIGKILL)
    except PermissionError, ProcessLookupError:
        pass
    if not leader_reaped:
        try:
            _, status, _ = os.wait4(process.pid, 0)
        except ChildProcessError:
            pass
        else:
            process.returncode = os.waitstatus_to_exitcode(status)

    deadline = time.monotonic() + 1
    while _process_group_exists(process_group) and time.monotonic() < deadline:
        time.sleep(0.01)


def _process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _rss_bytes(raw_max_rss: int) -> int:
    if sys.platform == "darwin":
        return raw_max_rss
    return raw_max_rss * 1024


def _resource_method() -> str:
    if sys.platform == "darwin":
        unit = "darwin-bytes"
    else:
        unit = "linux-kib-times-1024"
    return (
        "wait4:rusage-waited-command-and-reaped-descendants;"
        f"ru_maxrss={unit};"
        "rss_scope=max-process-high-water-not-simultaneous-tree-peak"
    )
