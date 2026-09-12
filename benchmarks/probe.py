"""Separate, untimed invocation diagnostics. Counts are observed exec calls.

The native shim retains argv[0] and byte arguments across the Python shebang
boundary. The runtime logs once then execs the lexical real executable. These
paths are distinct compiler identities and must never enter timed workflows.
"""

import json
import re
import shutil
import sys
from dataclasses import asdict, dataclass, replace
from pathlib import Path

from benchmarks.probe_health import HealthChannel, check_health, create_health
from benchmarks.process import Command, Measurement, checked
from benchmarks.records import read_json, write_json
from benchmarks.toolchains import Tool, Toolchain, sha256


@dataclass(frozen=True)
class Probe:
    path: Path
    events: Path
    real: str
    kind: str
    preparation: Measurement
    health: HealthChannel


@dataclass(frozen=True)
class Event:
    event_id: str
    tool_kind: str
    tool: str
    argv: tuple[str, ...]
    cwd: str
    pid: int
    observed_ns: int


@dataclass(frozen=True)
class Observation:
    count: int | None
    reason: str | None = None


@dataclass(frozen=True)
class DiagnosticSummary:
    counts: dict[str, int]
    queries: Observation
    preprocess: Observation
    bitcode_compilations: Observation
    cache_hits: Observation
    failures: tuple[str, ...]
    event_ids: tuple[str, ...]
    unobserved: dict[str, str]
    health_channels: tuple[HealthChannel, ...]
    health_evidence: dict[str, str]
    scope: str = "observed tool/driver exec invocations; excludes hidden subprocesses and threads"
    schema_version: int = 1

    @property
    def valid(self) -> bool:
        return not self.failures


@dataclass(frozen=True)
class DiagnosticSession:
    root: Path
    cache: Path
    events: Path
    config: Path
    toolchain: Toolchain
    environment: dict[str, str]
    probes: tuple[Probe, ...]

    @property
    def health(self) -> tuple[HealthChannel, ...]:
        return tuple(probe.health for probe in self.probes)


def create_probe(
    root: Path,
    real: Path,
    name: str,
    kind: str,
    toolchain: Toolchain,
    *,
    env: dict[str, str],
    events: Path | None = None,
) -> Probe:
    """Create one private probe and retain its real-clang preparation command."""
    if not name or Path(name).name != name or name in (".", ".."):
        raise ValueError("probe name must be a single filename")
    real = (
        real.absolute()
    )  # Preserve aliases: resolving clang++ breaks dispatch.
    if not real.is_file():
        raise ValueError(f"missing real tool: {real}")
    root.mkdir(parents=True, exist_ok=True)
    events = events or root / "events"
    events.mkdir(parents=True, exist_ok=True)
    runtime = root / "runtime.py"
    config = root / "probe.json"
    path = root / name
    for owned in (runtime, config, path, root / "shim.c"):
        if owned.exists():
            raise FileExistsError(owned)
    health = create_health(root)
    shutil.copyfile(Path(__file__).with_name("probe_runtime.py"), runtime)
    write_json(
        config,
        {
            "schema_version": 1,
            "real": str(real),
            "kind": kind,
            "events": str(events),
        },
    )

    # Octal escapes avoid C universal-character and hex run-on ambiguities.
    def literal(value: str) -> str:
        import os

        return (
            '"' + "".join(f"\\{byte:03o}" for byte in os.fsencode(value)) + '"'
        )

    source = root / "shim.c"
    source.write_text(
        "#include <unistd.h>\n#include <stdlib.h>\n#include <stdio.h>\n#include <fcntl.h>\n"
        "int main(int n,char **v){"
        f"int seal=open({literal(health.seal)},O_RDWR);"
        'if(seal<0){perror("diagnostic health seal");return 126;}'
        f"char receipt[]={literal(health.directory + '/attempt-XXXXXX')};"
        "int record=mkstemp(receipt);"
        f"int directory=open({literal(health.directory)},O_RDONLY);"
        "if(record<0||directory<0||fsync(record)||fsync(directory)){"
        'if(pwrite(seal,"!",1,0)!=1||fsync(seal))return 126;'
        "close(seal);if(record>=0)close(record);if(directory>=0)close(directory);"
        f"execv({literal(str(real))},v);return 126;}}"
        "close(record);close(directory);close(seal);"
        "char **a=calloc((size_t)n+5,sizeof(char*));"
        "if(!a)return 126;"
        f"a[0]={literal(sys.executable)};a[1]={literal(str(runtime))};"
        f"a[2]={literal(str(config))};"
        "a[3]=receipt;for(int i=0;i<n;i++)a[i+4]=v[i];"
        'execv(a[0],a);perror("diagnostic probe runtime");return 126;}\n'
    )
    preparation = checked(
        Command(
            (toolchain.path("clang"), str(source), "-O2", "-o", str(path)),
            root,
            dict(env),
        ),
        root / "logs",
        "prepare",
    )
    return Probe(path, events, str(real), kind, preparation, health)


def read_events(directory: Path) -> tuple[Event, ...]:
    events: list[Event] = []
    for path in sorted(directory.glob("event-*.json")):
        data = read_json(path)
        argv = data.get("argv")
        pid = data.get("pid")
        observed_ns = data.get("observed_ns")
        if not path.read_bytes().endswith(b"\n"):
            raise ValueError(f"incomplete probe event: {path}")
        if (
            data.get("event_id") != path.name
            or not isinstance(argv, list)
            or not argv
            or not all(isinstance(a, str) for a in argv)
            or type(pid) is not int
            or type(observed_ns) is not int
            or not all(
                isinstance(data.get(k), str)
                for k in ("tool_kind", "tool", "cwd")
            )
        ):
            raise ValueError(f"malformed probe event: {path}")
        events.append(
            Event(
                str(data["event_id"]),
                str(data["tool_kind"]),
                str(data["tool"]),
                tuple(argv),
                str(data["cwd"]),
                pid,
                observed_ns,
            )
        )
    return tuple(events)


def summarize_events(
    events: tuple[Event, ...],
    *,
    measurements: tuple[Measurement, ...] = (),
    cache_trace: bool = False,
    health: tuple[HealthChannel, ...] = (),
    prior_event_ids: frozenset[str] = frozenset(),
) -> DiagnosticSummary:
    """Summarize a parent-selected event slice, never changing the child env.

    Use event-ID set differences across phases. Response arguments deliberately
    make flag-derived details unavailable. A zero cache-hit count is justified
    only when a successful wrapper replay retained its verbose cache trace.
    """
    counts: dict[str, int] = {}
    failures: list[str] = []
    if not measurements:
        failures.append("missing diagnostic command completion evidence")
    healthy_events, health_evidence, health_failures = check_health(health)
    failures.extend(health_failures)
    if {
        event.event_id for event in events
    } != healthy_events.keys() - prior_event_ids:
        failures.append(
            "diagnostic event slice does not match durable health receipts"
        )
    if not prior_event_ids <= healthy_events.keys():
        failures.append(
            "prior diagnostic event identities lack healthy receipts"
        )
    for event in events:
        receipt_event = healthy_events.get(event.event_id, {})
        if any(
            receipt_event.get(key) != (list(value) if key == "argv" else value)
            for key, value in asdict(event).items()
        ):
            failures.append(
                f"diagnostic event differs from health evidence: {event.event_id}"
            )
    queries = preprocess = bitcode = 0
    opaque = False
    for event in events:
        counts[event.tool_kind] = counts.get(event.tool_kind, 0) + 1
        if event.tool_kind not in ("compiler-driver", "rustc"):
            continue
        args = event.argv[1:]
        opaque |= any(a.startswith("@") for a in args)
        queries += int(
            any(
                a
                in (
                    "--version",
                    "-vV",
                    "--help",
                    "-dumpmachine",
                    "-dumpversion",
                )
                or a.startswith(("-print-", "--print"))
                for a in args
            )
        )
        preprocess += int("-E" in args)
        bitcode += int("-emit-llvm" in args and "-c" in args)
    cache_hits = 0
    cache_decisions = 0
    for measurement in measurements:
        if measurement.returncode != 0:
            failures.append(f"diagnostic command failed: {measurement.argv}")
        stderr = Path(measurement.stderr).read_text(errors="replace")
        if "RLLVM_BENCHMARK_PROBE_ERROR:" in stderr:
            failures.append("diagnostic probe could not record an invocation")
        cache_hits += len(re.findall(r"\bCache hit: src=", stderr))
        cache_decisions += len(
            re.findall(r"\bCache (?:hit|miss): src=", stderr)
        )
    if not events:
        failures.append(
            "no tool invocations observed; instrumentation coverage unproven"
        )
    reason = (
        "response-file arguments hide compiler phase flags" if opaque else None
    )
    driver_observed = counts.get("compiler-driver", 0) > 0
    if not driver_observed and reason is None:
        reason = "no C/C++ driver observations; phase counts unavailable"
    return DiagnosticSummary(
        counts,
        Observation(None if opaque else queries, reason if opaque else None),
        Observation(None if reason else preprocess, reason),
        Observation(None if reason else bitcode, reason),
        Observation(
            cache_hits
            if cache_trace and cache_decisions and not failures
            else None,
            None
            if cache_trace and cache_decisions and not failures
            else "requires successful separate cache replay with verbose wrapper trace",
        ),
        tuple(failures),
        tuple(e.event_id for e in events),
        {
            kind: "no invocation of this tool observed in the selected phase; routing coverage is not established"
            for kind in ("compiler-driver", "rustc", "llvm-link", "llvm-ar")
            if kind not in counts
        },
        health,
        health_evidence,
    )


def prepare_diagnostics(
    root: Path,
    timed_cache: Path,
    toolchain: Toolchain,
    *,
    env: dict[str, str],
) -> DiagnosticSession:
    """Prepare isolated tool identities, cache/config, and a stable phase env.

    Caller schedules cold then primed replays using this same environment and
    records phase labels outside children. This function performs no replay.
    """
    root = root.resolve()
    timed_cache = timed_cache.resolve()
    if (
        root == timed_cache
        or root.is_relative_to(timed_cache)
        or timed_cache.is_relative_to(root)
    ):
        raise ValueError("diagnostic root must be separate from timed cache")
    root.mkdir(parents=True, exist_ok=False)
    cache, events, config = (
        root / "cache",
        root / "events",
        root / "rllvm.toml",
    )
    cache.mkdir()
    events.mkdir()
    probes: list[Probe] = []
    tools = dict(toolchain.tools)
    for name, kind in (
        ("clang", "compiler-driver"),
        ("clang++", "compiler-driver"),
        ("rustc", "rustc"),
        ("llvm-link", "llvm-link"),
        ("llvm-ar", "llvm-ar"),
    ):
        if name not in tools:
            continue
        probe = create_probe(
            root / name,
            Path(toolchain.path(name)),
            name,
            kind,
            toolchain,
            env=env,
            events=events,
        )
        probes.append(probe)
        tools[name] = Tool(
            str(probe.path),
            str(probe.path.resolve()),
            sha256(probe.path),
            toolchain.tools[name].version,
        )
    settings: dict[str, str | bool] = {
        "cache_enabled": True,
        "cache_dir": str(cache),
        "bitcode_store_path": str(root / "bitcode"),
    }
    for name, key in (
        ("clang", "clang"),
        ("clang++", "clangxx"),
        ("rustc", "rustc"),
        ("llvm-link", "llvm_link"),
        ("llvm-ar", "llvm_ar"),
        ("llvm-config", "llvm_config"),
        ("llvm-objcopy", "llvm_objcopy"),
    ):
        if name in tools:
            settings[key + "_filepath"] = tools[name].path
    config.write_text(
        "\n".join(f"{k} = {json.dumps(v)}" for k, v in settings.items()) + "\n"
    )
    environment = dict(env)
    environment.update(
        RLLVM_CONFIG=str(config),
        RLLVM_CACHE="1",
        RLLVM_BENCHMARK_EVENTS=str(events),
        RLLVM_LOG_LEVEL="trace",
    )
    session = DiagnosticSession(
        root,
        cache,
        events,
        config,
        replace(toolchain, tools=tools, environment=environment),
        environment,
        tuple(probes),
    )
    write_json(
        root / "session.json",
        {
            "schema_version": 1,
            "timed_cache_excluded": str(timed_cache),
            "config": str(config),
            "environment": environment,
            "tools": session.toolchain.manifest(),
            "probes": [
                asdict(p) | {"path": str(p.path), "events": str(p.events)}
                for p in probes
            ],
        },
    )
    return session
