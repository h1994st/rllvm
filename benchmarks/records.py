"""Read and write versioned benchmark records."""

import fcntl
import json
import os
import tempfile
from pathlib import Path

SCHEMA_VERSION = 1


class RecordError(ValueError):
    """A persisted benchmark record is malformed or unsupported."""


class IncompleteRecordError(RecordError):
    """The final JSONL record was interrupted before its newline."""


def write_json(path: Path, value: object) -> None:
    """Atomically replace a JSON file with a newline-terminated value."""
    encoded = _encode(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            dir=path.parent,
            prefix=f".{path.name}.",
            delete=False,
        ) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(encoded)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_path, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary_path is not None and temporary_path.exists():
            temporary_path.unlink()


def append_record(path: Path, value: object) -> None:
    """Append one complete JSONL record while excluding other writers."""
    encoded = _encode(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_APPEND | os.O_CREAT,
        0o644,
    )
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        with os.fdopen(descriptor, "ab", closefd=False) as output:
            output.write(encoded)
            output.flush()
            os.fsync(output.fileno())
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def read_json(path: Path) -> dict[str, object]:
    """Read one versioned JSON object."""
    try:
        value = json.loads(path.read_bytes())
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RecordError(
            f"malformed JSON record in {path}: {error}"
        ) from error
    _validate_record(value, str(path))
    assert isinstance(value, dict)
    return value


def read_records(path: Path) -> list[dict[str, object]]:
    """Read complete, versioned JSONL observations."""
    contents = path.read_bytes()
    if not contents:
        return []
    if not contents.endswith(b"\n"):
        raise IncompleteRecordError(
            f"incomplete final record in {path}: missing newline"
        )

    records: list[dict[str, object]] = []
    for line_number, line in enumerate(contents.splitlines(), start=1):
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RecordError(
                f"malformed record at {path}:{line_number}: {error}"
            ) from error
        _validate_record(value, f"{path}:{line_number}")
        assert isinstance(value, dict)
        records.append(value)
    return records


def _encode(value: object) -> bytes:
    text = json.dumps(
        value,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    return f"{text}\n".encode()


def _validate_record(value: object, location: str) -> None:
    if not isinstance(value, dict):
        raise RecordError(f"record at {location} must be a JSON object")
    version = value.get("schema_version")
    if type(version) is not int:
        raise RecordError(
            f"record at {location} has no integer schema version"
        )
    if version != SCHEMA_VERSION:
        raise RecordError(
            f"record at {location} has unsupported schema version {version}"
        )
