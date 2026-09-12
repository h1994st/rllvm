"""Durable per-invocation receipts, independent of captured tool stderr."""

import os
from dataclasses import dataclass
from pathlib import Path

from benchmarks.records import read_json
from benchmarks.toolchains import sha256


@dataclass(frozen=True)
class HealthChannel:
    directory: str
    seal: str
    seal_sha256: str
    directory_device: int
    directory_inode: int
    seal_device: int
    seal_inode: int
    seal_ctime_ns: int


def create_health(root: Path) -> HealthChannel:
    directory, seal = root / "health", root / "health-seal"
    directory.mkdir()
    with seal.open("xb") as stream:
        stream.write(os.urandom(32).hex().encode() + b"\n")
        stream.flush()
        os.fsync(stream.fileno())
    ds, ss = directory.stat(), seal.stat()
    return HealthChannel(
        str(directory),
        str(seal),
        sha256(seal),
        ds.st_dev,
        ds.st_ino,
        ss.st_dev,
        ss.st_ino,
        ss.st_ctime_ns,
    )


def check_health(
    channels: tuple[HealthChannel, ...],
) -> tuple[dict[str, dict[str, object]], dict[str, str], tuple[str, ...]]:
    """Read every receipt after replay children finish; never reset a failure.

    Empty/truncated receipts indicate interruption before durable event commit.
    A missing/replaced/modified seal or directory cannot establish health.
    """
    events: dict[str, dict[str, object]] = {}
    evidence: dict[str, str] = {}
    failures: list[str] = []
    if not channels:
        failures.append("missing independent diagnostic health channels")
    for channel in channels:
        directory, seal = Path(channel.directory), Path(channel.seal)
        try:
            ds, ss = directory.stat(), seal.stat()
            if (ds.st_dev, ds.st_ino) != (
                channel.directory_device,
                channel.directory_inode,
            ):
                raise ValueError("replaced health directory")
            if (ss.st_dev, ss.st_ino, ss.st_ctime_ns) != (
                channel.seal_device,
                channel.seal_inode,
                channel.seal_ctime_ns,
            ) or sha256(seal) != channel.seal_sha256:
                raise ValueError("modified or replaced health seal")
            evidence[str(seal)] = channel.seal_sha256
            for path in sorted(directory.iterdir()):
                if not path.name.startswith(
                    "attempt-"
                ) or not path.read_bytes().endswith(b"\n"):
                    raise ValueError(f"incomplete health receipt: {path}")
                receipt = read_json(path)
                evidence[str(path)] = sha256(path)
                if receipt.get("status") != "recorded":
                    raise ValueError(
                        f"failed event recording: {path}: {receipt.get('error')}"
                    )
                event_path = receipt.get("event_path")
                if not isinstance(event_path, str):
                    raise ValueError(
                        f"missing event path in health receipt: {path}"
                    )
                event = Path(event_path)
                if receipt.get("event_sha256") != sha256(event):
                    raise ValueError(
                        f"changed event after health receipt: {event}"
                    )
                data = read_json(event)
                identifier = data.get("event_id")
                if (
                    not isinstance(identifier, str)
                    or identifier != event.name
                    or identifier in events
                ):
                    raise ValueError(
                        f"invalid/duplicate health event identity: {event}"
                    )
                events[identifier] = data
        except (OSError, ValueError) as error:
            failures.append(
                f"diagnostic health unavailable or failed: {error}"
            )
    return events, evidence, tuple(failures)
