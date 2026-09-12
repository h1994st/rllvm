"""Standalone diagnostic runtime; copied beside the native argv-preserving shim."""

import hashlib
import json
import os
import sys
import tempfile
import time
from pathlib import Path


def main() -> None:
    config = json.loads(Path(sys.argv[1]).read_bytes())
    receipt = Path(sys.argv[2])
    argv = sys.argv[3:]
    events = os.environ.get("RLLVM_BENCHMARK_EVENTS", config["events"])
    try:
        descriptor, path = tempfile.mkstemp(
            prefix="event-", suffix=".json", dir=events
        )
        event = {
            "schema_version": 1,
            "event_id": Path(path).name,
            "tool_kind": config["kind"],
            "tool": config["real"],
            "argv": argv,
            "cwd": os.getcwd(),
            "pid": os.getpid(),
            "observed_ns": time.time_ns(),
        }
        # JSON escapes lone surrogates losslessly; argv passed to exec is never
        # re-encoded through JSON. O_EXCL filenames remain unique after PID reuse.
        with os.fdopen(descriptor, "w", encoding="ascii") as stream:
            json.dump(event, stream, ensure_ascii=True, allow_nan=False)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        status = {
            "schema_version": 1,
            "status": "recorded",
            "event_path": path,
            "event_sha256": hashlib.sha256(
                Path(path).read_bytes()
            ).hexdigest(),
        }
    except Exception as error:
        # Instrumentation must not change the child invocation. The diagnostic
        # controller treats this visible marker as invalid observation evidence.
        print(f"RLLVM_BENCHMARK_PROBE_ERROR: {error!r}", file=sys.stderr)
        status = {
            "schema_version": 1,
            "status": "failed",
            "error": repr(error),
        }
    try:
        descriptor, pending = tempfile.mkstemp(
            prefix="receipt-", dir=receipt.parent
        )
        with os.fdopen(descriptor, "w", encoding="ascii") as stream:
            json.dump(status, stream, ensure_ascii=True, allow_nan=False)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        # Publish only after the completed record is durable. A failed write,
        # flush, fsync, or replacement leaves the shim's empty receipt intact.
        os.replace(pending, receipt)
    except Exception as error:
        # The shim's durable pending receipt remains incomplete, independently
        # invalidating diagnostics even when this stderr is swallowed.
        print(f"RLLVM_BENCHMARK_PROBE_ERROR: {error!r}", file=sys.stderr)
    os.execv(config["real"], argv)


if __name__ == "__main__":
    main()
