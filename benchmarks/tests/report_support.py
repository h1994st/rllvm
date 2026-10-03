"""Saved workflow run records for offline report and baseline tests."""

from pathlib import Path
from typing import Any

from benchmarks.records import append_record, write_json
from benchmarks.workflow_contract import (
    DIAGNOSTIC_PHASES,
    required_sample_gates,
)


def saved_run(
    root: Path,
    *,
    source: str = "source-a",
    compiler: str = "compiler-a",
    flags: tuple[str, ...] = ("-DFEATURE=ON",),
    targets: tuple[str, ...] = ("static",),
    jobs: int = 2,
    wrapped_cache_state: str = "disabled",
    host_machine: str = "x86_64",
    host_processor: str = "fixture-cpu",
    host_identity: str = "fixture-host-a",
    sdkroot: str = "/sdk/one",
    planned: bool = False,
    wrapped_gate_evidence: dict[str, dict[str, Any]] | None = None,
) -> Path:
    """Write one saved run; wrapped gates gain `wrapped_gate_evidence`."""
    root.mkdir()
    samples: list[dict[str, Any]] = []
    for repetition, (native, wrapped) in enumerate(
        zip((2.0, 4.0, 6.0), (4.0, 8.0, 12.0), strict=True)
    ):
        for arm, wall in (("native", native), ("wrapped-uncached", wrapped)):
            samples.append(
                {
                    "schema_version": 1,
                    "id": f"{repetition:03d}-{arm}-clean",
                    "repetition": repetition,
                    "arm": arm,
                    "state": "clean",
                    "valid": not planned,
                    "complete": True,
                    "gates": {},
                    "errors": [],
                    "missing_gates": [],
                    "command_sequences": [repetition],
                    "phase_totals": {
                        "timed:clean-build": {
                            "commands": 1,
                            "wall_seconds": wall,
                            "user_cpu_seconds": wall / 2,
                            "system_cpu_seconds": 0.0,
                            "max_observed_process_rss_bytes": 1024,
                            "complete": not planned,
                            "rss_scope": "maximum observed command high-water; not tree peak",
                        },
                        "validation:api": {
                            "commands": 1,
                            "wall_seconds": 0.25,
                            "user_cpu_seconds": 0.1,
                            "system_cpu_seconds": 0.0,
                            "max_observed_process_rss_bytes": 512,
                            "complete": not planned,
                            "rss_scope": "maximum observed command high-water; not tree peak",
                        },
                    },
                    "disk": {
                        "native-and-build-artifacts": {
                            "logical_bytes": 10,
                            "allocated_bytes": 16,
                        },
                        "bitcode-store": {
                            "logical_bytes": 0 if arm == "native" else 20,
                            "allocated_bytes": 0 if arm == "native" else 24,
                        },
                    },
                    "cache_state": (
                        "disabled" if arm == "native" else wrapped_cache_state
                    ),
                    "build_artifact_state": "empty",
                    "cargo_artifact_state": None,
                }
            )
    invalid = dict(samples[-1])
    invalid.update(id="invalid", repetition=2, valid=False)
    phase_totals = samples[-1]["phase_totals"]
    assert isinstance(phase_totals, dict)
    clean_build = phase_totals["timed:clean-build"]
    assert isinstance(clean_build, dict)
    invalid["phase_totals"] = {
        "timed:clean-build": {
            **clean_build,
            "wall_seconds": 0.01,
        }
    }
    invalid["errors"] = ["validation failed"]
    samples.append(invalid)
    for sequence, sample in enumerate(samples, start=100):
        required = required_sample_gates(
            sample["arm"], sample["state"], targets, 2
        )
        gate_names = {"commands", "diagnostics"} if planned else required
        sample["command_sequences"] = [sequence]
        sample["gates"] = {
            name: {
                "valid": not (planned and name == "diagnostics"),
                "failures": ["dry run: diagnostic evidence unavailable"]
                if planned and name == "diagnostics"
                else [],
                "evidence": [],
            }
            for name in gate_names
        }
        if sample["arm"] != "native":
            for name, extra in (wrapped_gate_evidence or {}).items():
                sample["gates"][name] = {
                    "valid": True,
                    "failures": [],
                    "evidence": [],
                    **extra,
                }
        sample["missing_gates"] = sorted(required - gate_names)
        append_record(root / "samples.jsonl", sample)
        append_record(
            root / "commands.jsonl",
            {
                "schema_version": 1,
                "sequence": sequence,
                "repetition": sample["repetition"],
                "arm": sample["arm"],
                "state": sample["state"],
                "phase": "clean-build",
                "category": "timed",
                "command": {"argv": ["build"], "cwd": "/build", "env": {}},
                "measurement": None
                if planned
                else {
                    "returncode": 0,
                    "wall_seconds": sample["phase_totals"][
                        "timed:clean-build"
                    ]["wall_seconds"],
                    "user_cpu_seconds": 1.0,
                    "system_cpu_seconds": 0.0,
                    "max_process_rss_bytes": 1024,
                    "resource_method": "fixture",
                    "failure": None,
                },
            },
        )
        append_record(
            root / "operations.jsonl",
            {
                "schema_version": 1,
                "sequence": sequence + 1000,
                "operation": "command-start",
                "command_sequence": sequence,
            },
        )
        for gate, evidence in sample["gates"].items():
            append_record(
                root / "validations.jsonl",
                {
                    "schema_version": 1,
                    "sequence": sequence + 2000,
                    "sample": sample["id"],
                    "gate": gate,
                    **evidence,
                },
            )
    if not planned:
        for repetition in range(3):
            for phase in DIAGNOSTIC_PHASES:
                append_record(
                    root / "diagnostics.jsonl",
                    {
                        "schema_version": 1,
                        "repetition": repetition,
                        "phase": phase,
                        "valid": True,
                        "failures": [],
                        "summary": {
                            "schema_version": 1,
                            "failures": [],
                            "event_ids": ["event-a.json", "event-b.json"],
                            "unobserved": {
                                "rustc": "compiler count unavailable"
                            },
                        },
                    },
                )
    recipe = {
        "profile_id": "fixture-cmake",
        "project": "fixture",
        "build_system": "cmake",
        "repository_url": "https://example.invalid/fixture",
        "commit": source,
        "required_submodules": [],
        "cxx": False,
        "cmake_flags": list(flags),
        "autotools_flags": [],
        "build_targets": list(targets),
        "selected_target_id": targets[0],
        "edit": {"path": "version.c", "before": "a", "after": "b"},
        "lockfile": None,
    }
    toolchain = {
        "host": "linux",
        "tools": {
            "clang": {
                "path": "/tools/clang",
                "realpath": "/tools/clang",
                "sha256": compiler,
                "version": "clang 21",
            }
        },
        "environment": {
            "PATH": "/usr/bin",
            "SDKROOT": sdkroot,
            "DEVELOPER_DIR": "/developer/one",
            "RLLVM_CONFIG": str(root / "discovery/rllvm.toml"),
        },
        "dependencies": [],
        "records": [],
        "generator": "Ninja",
        "rust_host": None,
        "limitations": [],
    }
    fixture = {
        "schema_version": 1,
        "kind": "prepared-fixture",
        "identity": source,
        "source": "/private/output-dependent/source",
        "recipe": recipe,
        "commit": source,
        "tree": source,
        "submodules": {},
        "lock_sha256": None,
        "toolchain": toolchain,
        "preparation_records": [],
        "manifest_path": "/private/output-dependent/prepared.json",
        "targets": [{"id": target} for target in targets],
        "limitations": [],
    }
    manifest = {
        "schema_version": 1,
        "kind": "workflow-run",
        "status": "planned" if planned else "invalid",
        "valid": False,
        "options": {
            "root": str(root),
            "repetitions": 3,
            "jobs": jobs,
            "seed": 144,
            "extraction_repeats": 2,
            "diagnostics": True,
            "dry_run": planned,
            "rllvm_provenance": {},
        },
        "fixture": fixture,
        "toolchain": toolchain,
        "rllvm_provenance": {},
        "host": {
            "platform": "Linux-6.0",
            "machine": host_machine,
            "processor": host_processor,
            "cpu_count": 8,
            "stable_hardware": host_identity,
            "load_start": [1.0, 1.0, 1.0],
            "load_end": [2.0, 2.0, 2.0],
        },
        "limitations": ["filesystem cache uncontrolled"],
        "records": {
            "commands": "commands.jsonl",
            "operations": "operations.jsonl",
            "validations": "validations.jsonl",
            "samples": "samples.jsonl",
            "diagnostics": "diagnostics.jsonl",
        },
        "expected_samples": len(samples),
        "phase_totals": {
            "priming:prime-build": {
                "wall_seconds": 1.0,
                "complete": True,
            }
        },
        "result": {
            "root": str(root),
            "status": "planned" if planned else "invalid",
            "failed_samples": len(samples) if planned else 1,
            "sample_count": len(samples),
            "errors": [],
            "schema_version": 1,
        },
        "errors": [],
    }
    write_json(root / "run.json", manifest)
    return root / "run.json"
