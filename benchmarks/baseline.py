"""Package a compact, committable baseline from saved benchmark records.

Every value comes from saved run, report, and preparation records, or from
the post-measurement source audit. A value the records cannot supply stays
`null`, and the object holding it explains why under `unavailable`.
"""

import io
import json
import os
import shutil
import statistics
import subprocess
import tarfile
import tempfile
import textwrap
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO

from benchmarks.records import SCHEMA_VERSION, RecordError, read_json
from benchmarks.report import (
    ReportError,
    paired_ratios,
    read_saved_run,
    resolve_manifests,
    timed_wall_seconds,
)
from benchmarks.toolchains import sha256

ARCHIVE_NAME = "records.tar.xz"
ARCHIVE_STORAGE = "Retained separately; excluded from Git"
ARCHIVE_GROUP = "runs.json"
BUILD_PHASE_SUFFIX = "-build"
SOURCE_ROOT_LAYOUT = "EXAMPLES_ROOT/PROJECT"
REPORT_FILES = ("report.json", "report.md", "samples.csv")
TREATMENTS = (
    "native",
    "wrapped-uncached",
    "wrapped-empty-cache",
    "wrapped-primed-cache",
)
TREATMENT_HEADINGS = (
    "Native",
    "Wrapped, cache off",
    "Wrapped, empty cache",
    "Wrapped, primed cache",
)
EVIDENCE_SCOPE = (
    "Original run and preparation group manifests, report outputs, supplied "
    "context files, run manifests and all indexed JSONL streams; build trees "
    "and full LLVM/stdout logs remain outside Git."
)
KNOWN_LIMITATIONS = (
    "Tool hashes identify resolved executable files; Cargo/rustc proxies are "
    "additionally identified by version output.",
    "rllvm_binary_sha256 covers the rllvm binaries the harness resolved; "
    "other binaries from the build command are not recorded.",
    "Full commands, paths, flags, logs and original validation data are "
    "retained outside Git.",
)
# Per-event diagnostic evidence stays in the archive; the summary keeps counts.
DIAGNOSTIC_COUNTS = {
    "event_ids": "validated_event_count",
    "health_channels": "health_channel_count",
    "health_evidence": "health_evidence_file_count",
}
NO_NATIVE_PAIR = "no comparable valid native pair"
WRAP = 80


class BaselineError(ValueError):
    """Saved records cannot produce a trustworthy baseline."""


@dataclass(frozen=True)
class BaselineArtifacts:
    directory: Path
    readme: Path
    archive: Path


@dataclass(frozen=True)
class _Profile:
    summary: dict[str, Any]
    project: str
    commit: str
    edit: dict[str, Any]
    timed_phases: dict[str, set[str]]
    sample_count: int


def package_baseline(
    runs: Path,
    report: Path,
    prepared: Path,
    examples_root: Path,
    conditions: str,
    output: Path,
    context: tuple[Path, ...] = (),
) -> BaselineArtifacts:
    """Write the baseline directory and its records archive to `output`."""
    if output.exists() or output.is_symlink():
        raise BaselineError(f"output already exists: {output}")
    if not conditions.strip():
        raise BaselineError("host conditions must be supplied")
    try:
        return _package(
            runs, report, prepared, examples_root, conditions, output, context
        )
    except BaselineError, ReportError:
        raise
    except (KeyError, TypeError, ValueError) as error:
        raise BaselineError(f"malformed baseline records: {error}") from error


def _package(
    runs: Path,
    report: Path,
    prepared: Path,
    examples_root: Path,
    conditions: str,
    output: Path,
    context: tuple[Path, ...],
) -> BaselineArtifacts:
    group_path = runs / "run-group.json" if runs.is_dir() else runs
    group = _read(group_path, "workflow-run-group")
    report_path = report / "report.json" if report.is_dir() else report
    structured = _read(report_path, "workflow-report")
    prepared_group = _read(prepared, "prepared-fixture-group")
    prepared_identities = {
        entry["profile"]: entry["identity"]
        for entry in prepared_group["manifests"]
    }
    members = _input_members(group_path, prepared, report_path, context)

    rows: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in structured["samples"]:
        rows[row["profile"]].append(row)
    workflow: dict[tuple[str, str, str], dict[str, Any]] = {}
    for item in structured["summaries"]:
        key = (item["profile"], item["state"], item["treatment"])
        if key in workflow:
            raise BaselineError(
                "report has more than one summary for "
                + "/".join(key)
                + "; artifact states must agree within a state"
            )
        workflow[key] = item

    paths = resolve_manifests((group_path,))
    if structured["runs"] != len(paths):
        raise BaselineError(
            f"report covers {structured['runs']} runs, "
            f"but the run group has {len(paths)}"
        )
    profiles: dict[str, _Profile] = {}
    tools: dict[str, dict[str, Any]] = {}
    dependencies: dict[str, dict[str, Any]] = {}
    provenance: list[dict[str, Any]] = []
    options: list[dict[str, Any]] = []
    hosts: list[dict[str, Any]] = []
    limitations: dict[str, None] = {}
    for path in paths:
        run, streams = read_saved_run(path)
        manifest = run.manifest
        if manifest["status"] == "planned":
            raise BaselineError(f"a dry run cannot form a baseline: {path}")
        if run.profile in profiles:
            raise BaselineError(
                f"baseline needs one run per profile; {run.profile} repeats"
            )
        fixture = manifest["fixture"]
        if prepared_identities.get(run.profile) != fixture["identity"]:
            raise BaselineError(
                f"run {run.profile} was not measured from the supplied "
                f"prepared fixtures: {path}"
            )
        profile_rows = rows.pop(run.profile, [])
        _match_report(run.profile, run.samples, profile_rows)
        profiles[run.profile] = _profile(
            manifest,
            run.samples,
            run.diagnostics,
            streams["validations"],
            profile_rows,
            {
                key[1:]: value
                for key, value in workflow.items()
                if key[0] == run.profile
            },
        )
        _merge_tools(tools, manifest["toolchain"]["tools"], run.profile)
        _merge_dependencies(
            dependencies, manifest["toolchain"]["dependencies"], run.profile
        )
        provenance.append(manifest["rllvm_provenance"])
        options.append(manifest["options"])
        hosts.append(
            {
                key: value
                for key, value in manifest["host"].items()
                if not key.startswith("load_")
            }
        )
        limitations.update(dict.fromkeys(map(str, manifest["limitations"])))
        limitations.update(dict.fromkeys(map(str, fixture["limitations"])))
        members.append((f"runs/{run.profile}/run.json", path))
        for name, stream in sorted(run.stream_paths.items()):
            relative = manifest["records"][name]
            members.append((f"runs/{run.profile}/{relative}", stream))
        del run, streams
    if rows:
        raise BaselineError(
            "report contains samples for profiles outside the run group: "
            + ", ".join(sorted(rows))
        )
    for label, values in (
        ("rllvm provenance", provenance),
        ("host identity", hosts),
    ):
        if any(value != values[0] for value in values):
            raise BaselineError(f"profiles differ in {label}")
    shared_options = {
        key: options[0].get(key)
        for key in ("repetitions", "jobs", "seed", "extraction_repeats")
    }
    if any(
        {key: value.get(key) for key in shared_options} != shared_options
        for value in options
    ):
        raise BaselineError("profiles were run with different options")

    audit = _audit_sources(
        examples_root,
        {profile.project for profile in profiles.values()},
    )
    return _write(
        output,
        group,
        members,
        profiles,
        _provenance(provenance[0], tools, dependencies),
        audit,
        _Context(
            conditions=conditions,
            options=shared_options,
            prepared_profiles=list(prepared_group["profiles"]),
            limitations=list(limitations),
            context_files=len(context),
        ),
    )


def _read(path: Path, kind: str) -> dict[str, Any]:
    try:
        value = read_json(path)
    except (OSError, RecordError) as error:
        raise BaselineError(
            f"required record is unreadable: {error}"
        ) from error
    if value.get("kind") != kind:
        raise BaselineError(f"expected a {kind} record: {path}")
    return value


def _input_members(
    group: Path, prepared: Path, report: Path, context: tuple[Path, ...]
) -> list[tuple[str, Path]]:
    members = [
        ("original-groups/run-group.json", group),
        ("original-groups/prepared-group.json", prepared),
    ]
    members.extend(
        (f"report/{name}", report.parent / name)
        for name in REPORT_FILES
        if (report.parent / name).is_file()
    )
    names = [path.name for path in context]
    if len(names) != len(set(names)):
        raise BaselineError("context files must have distinct names")
    for path in context:
        if not path.is_file():
            raise BaselineError(f"context file is missing: {path}")
        members.append((f"context/{path.name}", path))
    return members


def _match_report(
    profile: str,
    samples: tuple[dict[str, Any], ...],
    rows: list[dict[str, Any]],
) -> None:
    by_id = {row["sample_id"]: row for row in rows}
    if len(by_id) != len(rows) or by_id.keys() != {s["id"] for s in samples}:
        raise BaselineError(
            f"report samples for {profile} do not match its saved run"
        )
    for sample in samples:
        row = by_id[sample["id"]]
        if (
            row["valid"] != sample["valid"]
            or row["repetition"] != sample["repetition"]
            or row["state"] != sample["state"]
            or row["treatment"] != sample["arm"]
            or json.loads(row["phase_totals"]) != sample["phase_totals"]
        ):
            raise BaselineError(
                f"report sample {profile}/{sample['id']} differs from its "
                "saved run"
            )


def _profile(
    manifest: dict[str, Any],
    samples: tuple[dict[str, Any], ...],
    diagnostics: tuple[dict[str, Any], ...],
    validations: tuple[dict[str, Any], ...],
    rows: list[dict[str, Any]],
    workflow: dict[tuple[str, str], dict[str, Any]],
) -> _Profile:
    fixture = manifest["fixture"]
    options = manifest["options"]
    unavailable = {}
    for key in ("started_utc", "ended_utc"):
        if not manifest.get(key):
            unavailable[key] = "run manifest records no timestamp"
    if not manifest["rllvm_provenance"]:
        unavailable["rllvm_provenance"] = (
            "run was recorded without --rllvm-provenance"
        )
    summary = {
        "build": _build_tables(rows),
        "workflow": _workflow_tables(workflow, rows),
        "commit": fixture["commit"],
        "tree": fixture["tree"],
        "host": manifest["host"],
        "jobs": options["jobs"],
        "repetitions": options["repetitions"],
        "rllvm_provenance": manifest["rllvm_provenance"],
        "started_utc": manifest.get("started_utc"),
        "ended_utc": manifest.get("ended_utc"),
        "valid_samples": sum(sample["valid"] for sample in samples),
        "coverage": _coverage(fixture, samples, validations),
        "diagnostics": [
            {
                "phase": record["phase"],
                "repetition": record["repetition"],
                "summary": _compact_diagnostics(record.get("summary")),
                "valid": record["valid"],
            }
            for record in diagnostics
        ],
    }
    if unavailable:
        summary["unavailable"] = unavailable
    timed_phases: dict[str, set[str]] = defaultdict(set)
    for row in rows:
        if row["valid"] and row["state"] == "clean":
            timed_phases[row["treatment"]].update(
                name
                for name in json.loads(row["phase_totals"])
                if name.startswith("timed:")
            )
    recipe = fixture["recipe"]
    return _Profile(
        summary,
        recipe["project"],
        fixture["commit"],
        recipe["edit"],
        timed_phases,
        len(samples),
    )


def _compact_diagnostics(summary: object) -> object:
    if not isinstance(summary, dict):
        return summary
    compact = dict(summary)
    for name, count in DIAGNOSTIC_COUNTS.items():
        if name in compact:
            compact[count] = len(compact.pop(name))
    return compact


def _build_tables(rows: list[dict[str, Any]]) -> dict[str, Any]:
    values: dict[tuple[str, str], list[tuple[int, float]]] = defaultdict(list)
    reasons: dict[tuple[str, str], list[str]] = defaultdict(list)
    for row in rows:
        key = (row["state"], row["treatment"])
        if not row["valid"]:
            reasons[key].append("invalid sample")
            continue
        total, missing = timed_wall_seconds(
            json.loads(row["phase_totals"]), phase_suffix=BUILD_PHASE_SUFFIX
        )
        if total is None:
            reasons[key].append(str(missing))
        else:
            values[key].append((row["repetition"], total))
    tables: dict[str, Any] = defaultdict(dict)
    for state, treatment in sorted(values.keys() | reasons.keys()):
        timed = values[state, treatment]
        raw = [value for _, value in timed]
        ratios = (
            paired_ratios(dict(values[state, "native"]), timed)
            if treatment != "native"
            else []
        )
        reason = "; ".join(dict.fromkeys(reasons[state, treatment]))
        tables[state][treatment] = _cell(
            treatment,
            statistics.median(ratios) if ratios else None,
            statistics.median(raw) if raw else None,
            [min(raw), max(raw)] if raw else None,
            raw,
            reason or "no valid samples",
        )
    return dict(tables)


def _workflow_tables(
    summaries: dict[tuple[str, str], dict[str, Any]],
    rows: list[dict[str, Any]],
) -> dict[str, Any]:
    timed: dict[tuple[str, str], list[float]] = defaultdict(list)
    for row in rows:
        if row["valid"] and row["timed_wall_seconds"] is not None:
            timed[row["state"], row["treatment"]].append(
                row["timed_wall_seconds"]
            )
    if summaries.keys() != {(row["state"], row["treatment"]) for row in rows}:
        raise BaselineError("report summaries do not cover its samples")
    tables: dict[str, Any] = defaultdict(dict)
    for (state, treatment), item in sorted(summaries.items()):
        if item["raw_timed_wall_seconds"] != timed[state, treatment]:
            raise BaselineError(
                f"report summary {state}/{treatment} differs from its samples"
            )
        tables[state][treatment] = _cell(
            treatment,
            item["median_paired_overhead_ratio"],
            item["median_timed_wall_seconds"],
            item["range_timed_wall_seconds"],
            item["raw_timed_wall_seconds"],
            item["missing_reason"] or "no valid samples",
        )
    return dict(tables)


def _cell(
    treatment: str,
    ratio: float | None,
    median: float | None,
    span: list[float] | None,
    raw: list[float],
    reason: str,
) -> dict[str, Any]:
    cell: dict[str, Any] = {
        "median_paired_ratio": ratio,
        "median_wall_seconds": median,
        "range_wall_seconds": span,
        "raw_wall_seconds": raw,
    }
    unavailable = {}
    if median is None:
        unavailable["median_wall_seconds"] = reason
    if treatment != "native" and ratio is None:
        unavailable["median_paired_ratio"] = (
            reason if median is None else NO_NATIVE_PAIR
        )
    if unavailable:
        cell["unavailable"] = unavailable
    return cell


def _coverage(
    fixture: dict[str, Any],
    samples: tuple[dict[str, Any], ...],
    validations: tuple[dict[str, Any], ...],
) -> dict[str, Any]:
    valid = {sample["id"] for sample in samples if sample["valid"]}
    modules: dict[str, set[int]] = defaultdict(set)
    definitions: dict[str, set[int]] = defaultdict(set)
    for record in validations:
        if (
            record.get("sample") not in valid
            or record.get("valid") is not True
        ):
            continue
        gate = str(record.get("gate"))
        counts = record.get("per_target_module_count")
        if gate == "extraction-set" and isinstance(counts, dict):
            for target, count in counts.items():
                modules[target].add(count)
        native = record.get("native_definitions")
        if gate.startswith("all:") and isinstance(native, list):
            definitions[gate.removeprefix("all:")].add(len(native))
    coverage = {}
    for target in sorted(target["id"] for target in fixture["targets"]):
        entry: dict[str, Any] = {
            "module_counts": sorted(modules[target]) or None,
            "native_project_definition_counts": sorted(definitions[target])
            or None,
        }
        unavailable = {}
        if not modules[target]:
            unavailable["module_counts"] = (
                "no valid extraction-set validation counted this target"
            )
        if not definitions[target]:
            unavailable["native_project_definition_counts"] = (
                "no valid all-target validation listed native definitions"
            )
        if unavailable:
            entry["unavailable"] = unavailable
        coverage[target] = entry
    return coverage


def _merge_tools(
    merged: dict[str, dict[str, Any]],
    tools: dict[str, dict[str, Any]],
    profile: str,
) -> None:
    for name, tool in sorted(tools.items()):
        identity = {
            "invocation_name": Path(tool["path"]).name,
            "resolved_name": Path(tool["realpath"]).name,
            "sha256": tool["sha256"],
            "version": tool["version"],
        }
        if merged.setdefault(name, identity) != identity:
            raise BaselineError(f"{profile} used a different {name}")


def _merge_dependencies(
    merged: dict[str, dict[str, Any]],
    dependencies: list[dict[str, Any]],
    profile: str,
) -> None:
    for dependency in dependencies:
        identity = {
            "library_sha256": {
                Path(path).name: digest
                for path, digest in sorted(dependency["libraries"].items())
            },
            "name": dependency["name"],
            "version": dependency["version"],
        }
        if merged.setdefault(dependency["name"], identity) != identity:
            raise BaselineError(
                f"{profile} used a different {dependency['name']}"
            )


def _provenance(
    rllvm: dict[str, Any],
    tools: dict[str, dict[str, Any]],
    dependencies: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "kind": "workflow-baseline-provenance",
        "revision": rllvm.get("revision"),
        "build_command": rllvm.get("build_command"),
        "rllvm_binary_sha256": {
            name: tool["sha256"]
            for name, tool in sorted(tools.items())
            if name.startswith("rllvm-")
        },
        "tools": dict(sorted(tools.items())),
        "dependencies": list(dependencies.values()),
        "known_limitations": list(KNOWN_LIMITATIONS),
    }
    unavailable = {
        key: "rllvm provenance supplied to the runs does not state it"
        for key in ("revision", "build_command")
        if result[key] is None
    }
    if unavailable:
        result["unavailable"] = unavailable
    return result


def _audit_sources(examples_root: Path, projects: set[str]) -> dict[str, Any]:
    audited = {}
    for project in sorted(projects):
        checkout = examples_root / project
        if not checkout.is_dir():
            raise BaselineError(f"source checkout is missing: {checkout}")
        audited[project] = {
            "commit": _git(checkout, "rev-parse", "HEAD").strip(),
            "status_porcelain": _git(checkout, "status", "--porcelain"),
        }
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "post-baseline-source-audit",
        "projects": audited,
        "source_root_layout": SOURCE_ROOT_LAYOUT,
    }


def _git(checkout: Path, *args: str) -> str:
    # Optional locks off: auditing must not refresh the checkout's index.
    environment = dict(os.environ, GIT_OPTIONAL_LOCKS="0")
    try:
        return subprocess.run(
            ("git", "-C", str(checkout), *args),
            capture_output=True,
            text=True,
            check=True,
            env=environment,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        raise BaselineError(
            f"source audit failed for {checkout}: {error}"
        ) from error


@dataclass(frozen=True)
class _Context:
    conditions: str
    options: dict[str, Any]
    prepared_profiles: list[str]
    limitations: list[str]
    context_files: int


def _write(
    output: Path,
    group: dict[str, Any],
    members: list[tuple[str, Path]],
    profiles: dict[str, _Profile],
    provenance: dict[str, Any],
    audit: dict[str, Any],
    context: _Context,
) -> BaselineArtifacts:
    output = output.absolute()
    staging = Path(
        tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent)
    )
    try:
        archive = staging / ARCHIVE_NAME
        evidence = _archive(archive, group, sorted(members))
        summary = {
            "schema_version": SCHEMA_VERSION,
            "kind": "workflow-baseline-summary",
            "profiles": {
                name: profile.summary
                for name, profile in sorted(profiles.items())
            },
            "records_archive": {
                "name": ARCHIVE_NAME,
                "bytes": archive.stat().st_size,
                "sha256": sha256(archive),
                "storage": ARCHIVE_STORAGE,
            },
        }
        documents = {
            "summary.json": summary,
            "provenance.json": provenance,
            "evidence-manifest.json": {
                "schema_version": SCHEMA_VERSION,
                "kind": "workflow-baseline-evidence",
                "files": evidence,
                "scope": EVIDENCE_SCOPE,
            },
            "source-preservation.json": audit,
        }
        for name, value in documents.items():
            (staging / name).write_text(
                json.dumps(value, indent=2, sort_keys=True) + "\n"
            )
        (staging / "README.md").write_text(
            _readme(summary, profiles, provenance, audit, context)
        )
        staging.chmod(0o755)  # mkdtemp creates a private directory.
        staging.rename(output)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return BaselineArtifacts(
        output, output / "README.md", output / ARCHIVE_NAME
    )


def _archive(
    archive: Path, group: dict[str, Any], members: list[tuple[str, Path]]
) -> dict[str, dict[str, Any]]:
    relative_group = dict(group)
    relative_group["runs"] = [
        dict(entry, manifest=f"runs/{entry['profile']}/run.json")
        for entry in group["runs"]
    ]
    encoded = (
        json.dumps(relative_group, indent=2, sort_keys=True) + "\n"
    ).encode()
    evidence = {}
    with tarfile.open(archive, "w:xz") as tar:
        _add(tar, ARCHIVE_GROUP, io.BytesIO(encoded), len(encoded))
        for name, path in members:
            size = path.stat().st_size
            evidence[name] = {"bytes": size, "sha256": sha256(path)}
            with path.open("rb") as stream:
                _add(tar, name, stream, size)
    return evidence


def _add(tar: tarfile.TarFile, name: str, stream: BinaryIO, size: int) -> None:
    # Fixed metadata keeps the archive a function of its contents.
    info = tarfile.TarInfo(name)
    info.size = size
    info.mode = 0o644
    tar.addfile(info, stream)


def _readme(
    summary: dict[str, Any],
    profiles: dict[str, _Profile],
    provenance: dict[str, Any],
    audit: dict[str, Any],
    context: _Context,
) -> str:
    entries = summary["profiles"]
    names = sorted(entries)
    first = entries[names[0]]
    host = first["host"]
    options = context.options
    total = sum(profile.sample_count for profile in profiles.values())
    valid = sum(entry["valid_samples"] for entry in entries.values())
    started = sorted(
        entry["started_utc"]
        for entry in entries.values()
        if entry["started_utc"]
    )
    ended = sorted(
        entry["ended_utc"] for entry in entries.values() if entry["ended_utc"]
    )
    title_date = started[0][:10] if started else "undated"
    states = {state for e in entries.values() for state in e["workflow"]}
    treatments = {
        treatment
        for e in entries.values()
        for table in e["workflow"].values()
        for treatment in table
    }
    lines = [
        f"# {host.get('cpu_model') or 'Unrecorded host'} baseline, "
        f"{title_date}",
        "",
        _fill(
            (
                f"All {valid} samples passed"
                if valid == total
                else f"{valid} of {total} samples passed; invalid samples "
                "are excluded from every table"
            )
            + f": {_count(options['repetitions'], 'repetition')} of "
            f"{_count(len(treatments), 'treatment')} and "
            f"{_count(len(states), 'build state')} for {_series(names)}."
        ),
        "",
        "## Conditions",
        "",
    ]
    revision = provenance["revision"]
    build = provenance["build_command"]
    lines.append(
        _bullet(
            (
                f"rllvm revision: `{revision}`"
                if revision
                else "rllvm revision: not recorded"
            )
            + (f", built with `{build}`" if build else "")
            + ". Binary hashes and tool versions are in "
            "[provenance.json](provenance.json)."
        )
    )
    memory = host.get("memory_bytes")
    lines.append(
        _bullet(
            f"{host.get('cpu_model') or 'CPU model not recorded'}, "
            f"{host.get('cpu_count')} logical CPUs, "
            + (
                f"{memory / 2**30:g} GiB RAM"
                if memory
                else "memory not recorded"
            )
            + f", `{host.get('platform')}`."
        )
    )
    compilers = _compilers(provenance["tools"])
    if compilers:
        lines.append(_bullet(f"Compilers: {compilers}."))
    lines.append(
        _bullet(
            f"{options['jobs']} build jobs, {options['repetitions']} "
            f"repetitions, seed {options['seed']}, and "
            f"{options['extraction_repeats']} repeated extraction passes."
        )
    )
    if started and ended:
        loads = "; ".join(
            f"{name} {_load(entries[name]['host'].get('load_start'))} to "
            f"{_load(entries[name]['host'].get('load_end'))}"
            for name in names
        )
        lines.append(
            _bullet(
                f"Profiles ran serially from `{started[0]}` to "
                f"`{ended[-1]}`. Recorded 1, 5 and 15 minute load averages "
                f"at each profile's start and end: {loads}."
            )
        )
    if context.limitations:
        lines.append(
            _bullet(
                "Recorded limitations: " + "; ".join(context.limitations) + "."
            )
        )
    lines.extend(
        [
            "",
            "Host conditions supplied when packaging, which the records "
            "cannot establish:",
            "",
            *_conditions(context.conditions),
            "",
            _fill(
                "These are observations for the recorded fixtures and "
                "conditions, not performance thresholds or a claim that a "
                "cache helps every workload. Compact timing samples, ranges, "
                "paired ratios, diagnostic counts, and coverage are in "
                "[summary.json](summary.json). Full per-command reports "
                "remain outside Git."
            ),
            "",
            "## Clean build only",
            "",
            _fill(
                "Cells show median wall seconds and the median of the paired "
                "ratios against native in the same repetition. Build time is "
                "the sum of each sample's `timed:*-build` phases; "
                "configuration, extraction, inspection, priming, validation, "
                "and diagnostics are excluded. The raw build figures for all "
                "three states are in [summary.json](summary.json)."
            ),
            "",
            *_table(entries, "build"),
            "",
            "## Complete timed clean workflow",
            "",
            _fill(
                "These totals sum every timed phase of a sample. "
                + _phases(profiles)
                + " Priming, validation, and diagnostic work remain separate."
            ),
            "",
            *_table(entries, "workflow"),
            "",
            _fill(
                "Unchanged and edited states are in "
                "[summary.json](summary.json). An unchanged native build is "
                "usually very short, while the wrapped workflow still "
                "performs the requested analysis operations. Its large "
                "workflow ratio should not be read as wrapper-only overhead "
                "for a no-op build."
            ),
            "",
            "## Validated coverage",
            "",
            _fill(
                "Module counts describe the observed extracted inputs. "
                "Definition counts are the project-owned native definitions "
                "checked against both wrapped native outputs and extracted IR. "
                "Dependencies, prebuilt runtimes, assembly, and dynamically "
                "linked library bodies retain the boundaries documented by "
                "each recipe; these counts do not claim complete source "
                "coverage for every dependency."
            ),
            "",
            "| Profile | Target | Modules | Project definitions |",
            "|---|---|---:|---:|",
        ]
    )
    for name in names:
        for target, counts in entries[name]["coverage"].items():
            lines.append(
                f"| {name} | {target} | "
                f"{_counts(counts['module_counts'])} | "
                f"{_counts(counts['native_project_definition_counts'])} |"
            )
    lines.extend(["", _fill(_edits(profiles) + " " + _audit(profiles, audit))])
    archive = summary["records_archive"]
    lines.extend(
        [
            "",
            "## Evidence and offline reproduction",
            "",
            _bullet(
                f"The externally retained `{archive['name']}` preserves the "
                "original run and preparation group manifests, each run's "
                "`run.json` and indexed JSONL streams (commands, operations, "
                "validations, diagnostics, and final samples), the report "
                "generated from them, and "
                f"{_count(context.context_files, 'supplied context file')}. "
                "[evidence-manifest.json](evidence-manifest.json) lists "
                "SHA256 hashes and byte "
                "lengths of the original files."
            ),
            _bullet(
                "Full build trees, bitcode, tool stdout/stderr, and "
                "diagnostic receipts remain outside Git. Their recorded paths "
                "and hashes identify retained local evidence; rerun the "
                "workflow to recreate these large artifacts elsewhere."
            ),
            "",
            _fill(
                f"The archive is {archive['bytes']:,} bytes; its SHA256 is "
                f"`{archive['sha256']}`. It is excluded from Git and retained "
                "separately. Given the archive, regenerate the full report "
                "without building anything:"
            ),
            "",
            "```bash",
            "records_dir=$(mktemp -d)",
            'tar -xJf "$RECORDS_ARCHIVE" -C "$records_dir"',
            f'uv run python -m benchmarks report "$records_dir/{ARCHIVE_GROUP}" \\',
            '  --output "$records_dir/regenerated"',
            "```",
            "",
            _fill(
                f"The archive's `{ARCHIVE_GROUP}` uses relative manifest "
                "references. Original group manifests and command records "
                "preserve their observed runtime paths."
            ),
            "",
            "## Repeating measurements",
            "",
            _fill(
                "Use the recorded revision and tool versions, and configure "
                "the explicit source, dependency, and tool roots described in "
                "the [benchmark guide](../../README.md)."
                + (
                    f" The original preparation selected "
                    f"{len(context.prepared_profiles)} profiles; only the "
                    f"{len(names)} below were measured."
                    if sorted(context.prepared_profiles) != names
                    else ""
                )
                + " Build provenance must describe the binaries actually used."
            ),
            "",
            "```bash",
            "uv run python -m benchmarks prepare \\",
            *(f"  --profile {name} \\" for name in context.prepared_profiles),
            '  --examples-root "$EXAMPLES_ROOT" \\',
            '  --dependencies-root "$DEPENDENCIES_ROOT" \\',
            '  --tool-root "$RLLVM_BIN" --tool-root "$LLVM_BIN" \\',
            '  --tool-root "$RUST_BIN" --tool-root "$BUILD_TOOLS_BIN" \\',
            '  --tool-root "$SYSTEM_BIN" --output "$PREPARED"',
            "",
            "uv run python -m benchmarks run \\",
            '  --manifest "$PREPARED/prepared-group.json" \\',
            *(f"  --profile {name} \\" for name in names),
            f"  --baseline --repetitions {options['repetitions']} "
            f"--jobs {options['jobs']} --seed {options['seed']} \\",
            f"  --extraction-repeats {options['extraction_repeats']} \\",
            '  --rllvm-provenance "$PROVENANCE" --output "$RUN_ROOT"',
            "```",
            "",
            _fill(
                "The full source, submodule, lockfile, flags, tool hashes, "
                "dependency versions, and effective commands are retained "
                "per run. Source pins are:"
            ),
            "",
            "| Project | Commit |",
            "|---|---|",
            *(
                f"| {project} | `{commit}` |"
                for project, commit in sorted(
                    {(p.project, p.commit) for p in profiles.values()}
                )
            ),
            "",
            _fill(
                "This directory was generated by `uv run python -m benchmarks "
                "baseline` from the saved records."
            ),
        ]
    )
    return "\n".join(lines) + "\n"


def _conditions(text: str) -> list[str]:
    return [
        _bullet(line[2:]) if line.startswith("- ") else _fill(line)
        for line in text.strip().splitlines()
    ]


def _table(entries: dict[str, Any], kind: str) -> list[str]:
    lines = [
        "| Profile | " + " | ".join(TREATMENT_HEADINGS) + " |",
        "|---|" + "---:|" * len(TREATMENTS),
    ]
    for name, entry in sorted(entries.items()):
        clean = entry[kind].get("clean", {})
        cells = []
        for treatment in TREATMENTS:
            cell = clean.get(treatment)
            median = None if cell is None else cell["median_wall_seconds"]
            if median is None:
                cells.append("unavailable")
                continue
            ratio = cell["median_paired_ratio"]
            cells.append(
                f"{median:.3f} s"
                + (f" ({ratio:.2f}×)" if ratio is not None else "")
            )
        lines.append(f"| {name} | " + " | ".join(cells) + " |")
    return lines


def _compilers(tools: dict[str, dict[str, Any]]) -> str:
    parts = []
    for name in ("clang", "rustc"):
        if name not in tools:
            continue
        version = str(tools[name]["version"]).splitlines()
        text = f"`{version[0]}`" if version else f"`{name}`"
        llvm = next(
            (
                line.partition(":")[2].strip()
                for line in version
                if line.startswith("LLVM version:")
            ),
            None,
        )
        parts.append(text + (f" using LLVM {llvm}" if llvm else ""))
    return "; ".join(parts)


def _phases(profiles: dict[str, _Profile]) -> str:
    native: set[str] = set()
    wrapped: set[str] = set()
    for profile in profiles.values():
        for treatment, phases in profile.timed_phases.items():
            (native if treatment == "native" else wrapped).update(phases)
    if not native and not wrapped:
        return "No valid clean samples recorded timed phases."
    text = f"Native samples time {_codes(native)}."
    extra = wrapped - native
    if extra:
        text += f" Wrapped samples additionally time {_codes(extra)}."
    return text


def _edits(profiles: dict[str, _Profile]) -> str:
    edits = sorted(
        {
            (p.project, p.edit["path"], p.edit.get("expected_suffix") or "")
            for p in profiles.values()
        }
    )
    return (
        "The controlled source edit changes "
        + _series(
            [
                f"`{path}` in {project}"
                + (f" to add `{suffix}`" if suffix else "")
                for project, path, suffix in edits
            ]
        )
        + " in a private source snapshot. Native API checks and extracted "
        "LLVM string-constant checks require the changed value; the source "
        "is restored afterward."
    )


def _audit(profiles: dict[str, _Profile], audit: dict[str, Any]) -> str:
    pins = {p.project: p.commit for p in profiles.values()}
    findings = []
    for project, state in sorted(audit["projects"].items()):
        notes = []
        if state["commit"] != pins[project]:
            notes.append(f"at `{state['commit']}` rather than its pin")
        entries = len(state["status_porcelain"].splitlines())
        if entries:
            notes.append(
                f"with {_count(entries, 'changed or untracked path')}"
            )
        if notes:
            findings.append(f"{project} " + " and ".join(notes))
    if not findings:
        return (
            "The [original example checkouts](source-preservation.json) were "
            "clean at their pinned commits when this baseline was packaged."
        )
    return (
        "When this baseline was packaged, the [source audit]"
        "(source-preservation.json) of the original example checkouts found "
        + _series(findings)
        + ". Measurements used the prepared snapshots at the pinned commits "
        "listed below."
    )


def _counts(values: list[int] | None) -> str:
    return ", ".join(map(str, values)) if values else "unavailable"


def _load(values: object) -> str:
    if not isinstance(values, list | tuple) or not values:
        return "unrecorded"
    return "/".join(f"{value:.2f}" for value in values)


def _codes(names: set[str]) -> str:
    return _series([f"`{name}`" for name in sorted(names)])


def _count(value: int, noun: str) -> str:
    return f"{value} {noun}" + ("" if value == 1 else "s")


def _series(items: list[str]) -> str:
    if len(items) <= 2:
        return " and ".join(items)
    return ", ".join(items[:-1]) + ", and " + items[-1]


def _fill(text: str) -> str:
    return textwrap.fill(
        text, WRAP, break_long_words=False, break_on_hyphens=False
    )


def _bullet(text: str) -> str:
    return textwrap.fill(
        text,
        WRAP,
        initial_indent="- ",
        subsequent_indent="  ",
        break_long_words=False,
        break_on_hyphens=False,
    )
