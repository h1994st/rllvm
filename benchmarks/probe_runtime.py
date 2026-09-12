"""Standalone diagnostic runtime; copied beside the native argv-preserving shim."""

import json
import os
import sys
import tempfile
import time
from pathlib import Path


def main() -> None:
    config = json.loads(Path(sys.argv[1]).read_bytes())
    argv = sys.argv[2:]
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
    except Exception as error:
        # Instrumentation must not change the child invocation. The diagnostic
        # controller treats this visible marker as invalid observation evidence.
        print(f"RLLVM_BENCHMARK_PROBE_ERROR: {error!r}", file=sys.stderr)
    os.execv(config["real"], argv)


if __name__ == "__main__":
    main()
