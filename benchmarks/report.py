"""Deterministic offline reports for persisted workflow benchmark runs."""

import csv
import io
import json
import math
import statistics
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from benchmarks.records import RecordError, read_json, read_records


class ReportError(ValueError):
    """Saved benchmark evidence cannot produce a trustworthy report."""


@dataclass(frozen=True)
class ReportArtifacts:
    markdown: Path
    csv: Path
    json: Path


@dataclass(frozen=True)
class _SavedRun:
    manifest: dict[str, Any]
    samples: tuple[dict[str, Any], ...]
    commands: tuple[dict[str, Any], ...]
    diagnostics: tuple[dict[str, Any], ...]
    identity: tuple[object, ...]
    profile: str
    token: int


def generate_report(
    manifests: tuple[Path, ...] | list[Path], output: Path
) -> ReportArtifacts:
    """Generate stable Markdown, CSV and JSON without executing any commands."""
    try:
        paths = _resolve_manifests(tuple(manifests))
        if not paths:
            raise ReportError("at least one workflow run manifest is required")
        runs = tuple(
            _load_run(path, index) for index, path in enumerate(paths)
        )
        _validate_compatible_profiles(runs)
        markdown, rows, structured = _render(runs)
    except ReportError:
        raise
    except (KeyError, TypeError, ValueError) as error:
        raise ReportError(f"malformed workflow records: {error}") from error
    output = output.absolute()
    output.mkdir(parents=True, exist_ok=True)
    artifacts = ReportArtifacts(
        output / "report.md",
        output / "samples.csv",
        output / "report.json",
    )
    existing = [path for path in artifacts.__dict__.values() if path.exists()]
    if existing:
        raise ReportError(f"report artifact already exists: {existing[0]}")
    artifacts.markdown.write_text(markdown)
    artifacts.csv.write_text(_csv(rows))
    artifacts.json.write_text(
        json.dumps(structured, indent=2, sort_keys=True) + "\n"
    )
    return artifacts


def _resolve_manifests(paths: tuple[Path, ...]) -> tuple[Path, ...]:
    resolved: list[Path] = []
    for supplied in paths:
        path = supplied.absolute()
        if path.is_dir():
            group = path / "run-group.json"
            path = group if group.is_file() else path / "run.json"
        try:
            value = read_json(path)
        except (OSError, RecordError) as error:
            raise ReportError(str(error)) from error
        kind = value.get("kind")
        if kind == "workflow-run":
            resolved.append(path)
            continue
        if kind != "workflow-run-group":
            raise ReportError(
                f"unsupported report manifest kind in {path}: {kind}"
            )
        entries = value.get("runs")
        if not isinstance(entries, list) or not entries:
            raise ReportError(f"run group has no resolved manifests: {path}")
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(
                entry.get("manifest"), str
            ):
                raise ReportError(f"malformed run group entry in {path}")
            child = Path(entry["manifest"])
            resolved.append(
                child if child.is_absolute() else path.parent / child
            )
    return tuple(resolved)


def _load_run(path: Path, token: int) -> _SavedRun:
    try:
        manifest = read_json(path)
    except (OSError, RecordError) as error:
        raise ReportError(str(error)) from error
    if manifest.get("kind") != "workflow-run":
        raise ReportError(f"record is not a workflow run: {path}")
    status = manifest.get("status")
    if status == "incomplete":
        raise ReportError(f"workflow run is incomplete: {path}")
    if status not in {"valid", "invalid", "interrupted", "planned"}:
        raise ReportError(f"unknown workflow run status in {path}: {status}")
    for key in (
        "options",
        "fixture",
        "toolchain",
        "records",
        "expected_samples",
        "result",
        "phase_totals",
    ):
        if key not in manifest:
            raise ReportError(f"workflow run is missing {key}: {path}")
    fixture = _mapping(manifest["fixture"], "fixture", path)
    if (
        fixture.get("schema_version") != 1
        or fixture.get("kind") != "prepared-fixture"
    ):
        raise ReportError(
            f"workflow run has malformed fixture identity: {path}"
        )
    recipe = _mapping(fixture.get("recipe"), "fixture recipe", path)
    profile = recipe.get("profile_id")
    if not isinstance(profile, str) or not profile:
        raise ReportError(f"workflow run has no profile id: {path}")
    records = _mapping(manifest["records"], "record index", path)
    expected_streams = {
        "commands",
        "operations",
        "validations",
        "diagnostics",
        "samples",
    }
    if set(records) != expected_streams:
        raise ReportError(
            f"workflow run has incomplete or unknown record index: {path}"
        )
    stream_paths = {
        name: _optional_record_path(path, records[name], name)
        for name in expected_streams
    }
    samples_path = stream_paths["samples"]
    assert samples_path is not None
    if not samples_path.is_file():
        raise ReportError(f"workflow run is missing samples records: {path}")
    try:
        streams = {
            name: tuple(read_records(stream_path))
            if stream_path is not None and stream_path.exists()
            else ()
            for name, stream_path in stream_paths.items()
        }
    except (OSError, RecordError) as error:
        raise ReportError(str(error)) from error
    samples = streams["samples"]
    diagnostics = streams["diagnostics"]
    expected = manifest["expected_samples"]
    if type(expected) is not int or expected < 0 or len(samples) != expected:
        raise ReportError(
            f"workflow run sample stream is incomplete: expected {expected}, "
            f"found {len(samples)} in {samples_path}"
        )
    result = _mapping(manifest["result"], "result", path)
    if (
        result.get("schema_version") != 1
        or result.get("status") != status
        or result.get("sample_count") != expected
    ):
        raise ReportError(
            f"workflow run has inconsistent final result: {path}"
        )
    seen: set[str] = set()
    for sample in samples:
        _validate_sample(sample, samples_path)
        identity = sample["id"]
        assert isinstance(identity, str)
        if identity in seen:
            raise ReportError(
                f"duplicate sample id in {samples_path}: {identity}"
            )
        seen.add(identity)
    identity = _workload_identity(manifest, fixture, recipe)
    return _SavedRun(
        manifest,
        samples,
        streams["commands"],
        diagnostics,
        identity,
        profile,
        token,
    )


def _mapping(value: object, label: str, path: Path) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReportError(f"workflow run has malformed {label}: {path}")
    return value


def _optional_record_path(
    path: Path, value: object, label: str
) -> Path | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise ReportError(f"workflow run has malformed {label} path: {path}")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise ReportError(f"workflow run has unsafe {label} path: {value}")
    return path.parent / relative


def _validate_sample(sample: dict[str, Any], path: Path) -> None:
    required = {
        "id": str,
        "repetition": int,
        "arm": str,
        "state": str,
        "valid": bool,
        "complete": bool,
        "gates": dict,
        "errors": list,
        "missing_gates": list,
        "phase_totals": dict,
        "disk": dict,
        "cache_state": str,
        "build_artifact_state": str,
    }
    for key, expected in required.items():
        value = sample.get(key)
        if not isinstance(value, expected) or (
            expected is int and isinstance(value, bool)
        ):
            raise ReportError(f"malformed sample field {key} in {path}")
    if sample["state"] not in {"clean", "unchanged", "edited"}:
        raise ReportError(f"unknown sample state in {path}: {sample['state']}")
    if sample["arm"] not in {
        "native",
        "wrapped-uncached",
        "wrapped-empty-cache",
        "wrapped-primed-cache",
    }:
        raise ReportError(
            f"unknown sample treatment in {path}: {sample['arm']}"
        )


def _workload_identity(
    manifest: dict[str, Any],
    fixture: dict[str, Any],
    recipe: dict[str, Any],
) -> tuple[object, ...]:
    toolchain = _mapping(manifest["toolchain"], "toolchain", Path("run"))
    tools = _mapping(toolchain.get("tools"), "tools", Path("run"))
    tool_identities = []
    for name, value in sorted(tools.items()):
        tool = _mapping(value, f"tool {name}", Path("run"))
        tool_identities.append((name, tool.get("sha256"), tool.get("version")))
    targets = fixture.get("targets")
    options = _mapping(manifest["options"], "options", Path("run"))
    normalized_recipe = dict(recipe)
    normalized_recipe.pop("lockfile", None)
    dependencies = []
    raw_dependencies = toolchain.get("dependencies", [])
    if not isinstance(raw_dependencies, list):
        raise ReportError("workflow run has malformed tool dependencies")
    for raw in raw_dependencies:
        dependency = raw if isinstance(raw, dict) else {}
        libraries = dependency.get("libraries", {})
        prefix = str(dependency.get("prefix", ""))
        dependencies.append(
            (
                dependency.get("name"),
                dependency.get("version"),
                tuple(
                    sorted(
                        (Path(str(path)).name, digest)
                        for path, digest in libraries.items()
                    )
                )
                if isinstance(libraries, dict)
                else (),
                str(dependency.get("cflags", "")).replace(prefix, "{prefix}")
                if prefix
                else dependency.get("cflags", ""),
                str(dependency.get("libs", "")).replace(prefix, "{prefix}")
                if prefix
                else dependency.get("libs", ""),
            )
        )
    return (
        fixture.get("commit"),
        fixture.get("tree"),
        _canonical(fixture.get("submodules")),
        fixture.get("lock_sha256"),
        _canonical(normalized_recipe),
        _canonical(targets),
        tuple(tool_identities),
        tuple(dependencies),
        toolchain.get("host"),
        toolchain.get("generator"),
        toolchain.get("rust_host"),
        _canonical(manifest.get("rllvm_provenance", {})),
        options.get("jobs"),
        options.get("extraction_repeats"),
        options.get("diagnostics"),
    )


def _canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _validate_compatible_profiles(runs: tuple[_SavedRun, ...]) -> None:
    identities: dict[str, tuple[object, ...]] = {}
    treatments: dict[tuple[str, str, str], tuple[object, ...]] = {}
    phase_sets: dict[tuple[str, str, str], tuple[str, ...]] = {}
    for run in runs:
        previous = identities.setdefault(run.profile, run.identity)
        if previous != run.identity:
            raise ReportError(
                f"incompatible saved workloads for profile {run.profile}: "
                "source, compiler, flags, targets, jobs, or configuration differ"
            )
        for sample in run.samples:
            key = (run.profile, sample["state"], sample["arm"])
            treatment = (
                sample["cache_state"],
                sample["build_artifact_state"],
                sample.get("cargo_artifact_state"),
            )
            old = treatments.setdefault(key, treatment)
            if old != treatment:
                raise ReportError(
                    f"incompatible saved workloads for profile {run.profile}: "
                    "cache or artifact state differs"
                )
            if sample["valid"]:
                timed_phases = tuple(
                    sorted(
                        name
                        for name in sample["phase_totals"]
                        if name.startswith("timed:")
                    )
                )
                previous_phases = phase_sets.setdefault(key, timed_phases)
                if previous_phases != timed_phases:
                    raise ReportError(
                        f"incompatible saved workloads for profile {run.profile}: "
                        "timed phases differ"
                    )


def _timed(sample: dict[str, Any]) -> tuple[float | None, str | None]:
    values = []
    for name, raw in sample["phase_totals"].items():
        if not name.startswith("timed:"):
            continue
        if not isinstance(raw, dict):
            return None, f"malformed phase total {name}"
        value = raw.get("wall_seconds")
        if not raw.get("complete"):
            return None, f"incomplete phase {name}"
        if not isinstance(value, int | float) or isinstance(value, bool):
            return None, f"wall time unavailable for {name}"
        if not math.isfinite(value):
            return None, f"non-finite wall time for {name}"
        values.append(float(value))
    if not values:
        return None, "no timed phase totals"
    return sum(values), None


def _render(runs: tuple[_SavedRun, ...]) -> tuple[str, list[dict], dict]:
    lines = [
        "# rllvm workflow benchmark report",
        "",
        "Generated offline from versioned saved records. No builds, probes, or "
        "benchmark commands are executed during reporting.",
        "",
        "## Failures, excluded samples, and not-reached work",
        "",
    ]
    failures = _failures(runs)
    lines.extend(
        failures or ["No failures or excluded samples were recorded."]
    )
    lines.extend(["", "## Performance comparisons", ""])
    rows: list[dict] = []
    summaries = []
    grouped: dict[tuple[str, str, str, str | None], list[tuple]] = defaultdict(
        list
    )
    for run in runs:
        planned = run.manifest["status"] == "planned"
        for sample in run.samples:
            total, missing = _timed(sample)
            key = (
                run.profile,
                sample["state"],
                sample["build_artifact_state"],
                sample.get("cargo_artifact_state"),
            )
            grouped[key].append((run, sample, total, missing, planned))
            rows.append(_sample_row(run, sample, total, missing, planned))
    for key in sorted(grouped):
        profile, state, build_state, cargo_state = key
        values = grouped[key]
        lines.extend(
            [
                f"### {profile}: {state}",
                "",
                f"Build artifact state: `{build_state}`; Cargo artifact state: "
                f"`{cargo_state if cargo_state is not None else 'not-applicable'}`.",
                "",
                "| Treatment | Valid raw timed wall (s) | Median | Range | "
                "Paired overhead vs native | Missing/excluded |",
                "|---|---:|---:|---:|---:|---|",
            ]
        )
        by_arm: dict[str, list[tuple]] = defaultdict(list)
        for item in values:
            by_arm[item[1]["arm"]].append(item)
        native = by_arm.get("native", [])
        for arm in sorted(by_arm, key=lambda name: (name != "native", name)):
            items = by_arm[arm]
            valid = [
                value
                for _, sample, value, missing, planned in items
                if sample["valid"]
                and not planned
                and missing is None
                and value is not None
            ]
            excluded = len(items) - len(valid)
            reason = _missing_reason(items)
            ratio, paired = (
                _paired_ratio(native, items) if arm != "native" else (None, [])
            )
            planned = any(item[4] for item in items)
            ratio_text = (
                "unavailable (planned dry run)"
                if planned
                else "unavailable (no comparable valid pair)"
                if ratio is None
                else f"{_fmt(ratio)}x; raw "
                + ", ".join(_fmt(x) for x in paired)
            )
            raw = ", ".join(_fmt(value) for value in valid) or "unavailable"
            median = _fmt(statistics.median(valid)) if valid else "unavailable"
            range_text = (
                f"{_fmt(min(valid))}–{_fmt(max(valid))}"
                if valid
                else "unavailable"
            )
            missing_text = str(excluded)
            if reason:
                missing_text += f" ({reason})"
            lines.append(
                f"| {arm} | {raw} | {median} | {range_text} | "
                f"{ratio_text} | {missing_text} |"
            )
            summaries.append(
                {
                    "profile": profile,
                    "state": state,
                    "build_artifact_state": build_state,
                    "cargo_artifact_state": cargo_state,
                    "treatment": arm,
                    "raw_timed_wall_seconds": valid,
                    "median_timed_wall_seconds": statistics.median(valid)
                    if valid
                    else None,
                    "range_timed_wall_seconds": [min(valid), max(valid)]
                    if valid
                    else None,
                    "paired_overhead_ratios": paired,
                    "median_paired_overhead_ratio": ratio,
                    "excluded_samples": excluded,
                    "missing_reason": reason,
                }
            )
        lines.append("")
    lines.extend(_phase_section(runs))
    lines.extend(_missing_metrics_section(runs))
    lines.extend(_disk_section(runs))
    lines.extend(_diagnostic_section(runs))
    lines.extend(_coverage_section(runs))
    structured = {
        "schema_version": 1,
        "kind": "workflow-report",
        "runs": len(runs),
        "summaries": summaries,
        "samples": rows,
        "limitations": sorted(
            {
                str(item)
                for run in runs
                for item in run.manifest.get("limitations", [])
            }
        ),
    }
    return "\n".join(lines).rstrip() + "\n", rows, structured


def _failures(runs: tuple[_SavedRun, ...]) -> list[str]:
    result = []
    for run in runs:
        for error in run.manifest.get("errors", []):
            result.append(f"- `{run.profile}` run failure: {error}")
        for sample in run.samples:
            reasons = _sample_failures(sample)
            reasons += [
                f"missing gate: {value}" for value in sample["missing_gates"]
            ]
            if not sample["complete"] and not reasons:
                reasons.append("not reached or incomplete")
            if not sample["valid"]:
                value, missing = _timed(sample)
                timing = _fmt(value) + " s" if value is not None else missing
                result.append(
                    f"- `{run.profile}/{sample['id']}` excluded ({timing}): "
                    + (
                        "; ".join(reasons)
                        if reasons
                        else "validity gates failed"
                    )
                )
    return result


def _sample_failures(sample: dict[str, Any]) -> list[str]:
    reasons = [str(value) for value in sample["errors"]]
    for name, raw in sample["gates"].items():
        gate = raw if isinstance(raw, dict) else {}
        if gate.get("valid") is False:
            failures = gate.get("failures", [])
            if isinstance(failures, list) and failures:
                reasons.extend(f"{name}: {value}" for value in failures)
            else:
                reasons.append(f"{name}: validity gate failed")
    return reasons


def _missing_reason(items: list[tuple]) -> str | None:
    reasons = []
    for _, sample, _, missing, planned in items:
        if planned:
            reasons.append("planned dry run")
        elif not sample["valid"]:
            reasons.append("invalid sample")
        elif missing:
            reasons.append(missing)
    return "; ".join(dict.fromkeys(reasons)) or None


def _paired_ratio(native: list[tuple], wrapped: list[tuple]) -> tuple:
    native_values = {
        (run.token, sample["repetition"]): value
        for run, sample, value, missing, planned in native
        if sample["valid"]
        and not planned
        and missing is None
        and value is not None
    }
    ratios = []
    for run, sample, value, missing, planned in wrapped:
        base = native_values.get((run.token, sample["repetition"]))
        if (
            sample["valid"]
            and not planned
            and missing is None
            and value is not None
            and base is not None
            and base != 0
        ):
            ratios.append(value / base)
    return (statistics.median(ratios) if ratios else None, ratios)


def _sample_row(
    run: _SavedRun,
    sample: dict[str, Any],
    total: float | None,
    missing: str | None,
    planned: bool,
) -> dict:
    reason = (
        "planned dry run"
        if planned
        else "; ".join(_sample_failures(sample))
        if _sample_failures(sample)
        else missing
        if missing
        else "invalid validity gates"
        if not sample["valid"]
        else ""
    )
    return {
        "profile": run.profile,
        "sample_id": sample["id"],
        "repetition": sample["repetition"],
        "state": sample["state"],
        "treatment": sample["arm"],
        "cache_state": sample["cache_state"],
        "build_artifact_state": sample["build_artifact_state"],
        "cargo_artifact_state": sample.get("cargo_artifact_state"),
        "valid": sample["valid"],
        "complete": sample["complete"],
        "timed_wall_seconds": total,
        "missing_or_excluded_reason": reason,
        "phase_totals": _canonical(sample["phase_totals"]),
        "disk": _canonical(sample["disk"]),
    }


def _phase_section(runs: tuple[_SavedRun, ...]) -> list[str]:
    lines = [
        "## Phase totals and cumulative workflow costs",
        "",
        "Timed, priming, validation, and diagnostic categories remain separate. "
        "Cumulative values below come directly from each run manifest.",
        "",
        "| Scope | Category and phase | Wall (s) | Max observed RSS "
        "(bytes) | Complete |",
        "|---|---|---:|---:|---|",
    ]
    for run in runs:
        for sample in run.samples:
            for phase, value in sorted(sample["phase_totals"].items()):
                total = value if isinstance(value, dict) else {}
                lines.append(
                    f"| {run.profile}/{sample['id']} | {phase} | "
                    f"{_fmt(total.get('wall_seconds'))} | "
                    f"{_fmt(total.get('max_observed_process_rss_bytes'))} | "
                    f"{'yes' if total.get('complete') else 'no'} |"
                )
        totals = _mapping(
            run.manifest["phase_totals"], "phase totals", Path("run")
        )
        if not totals:
            lines.append(
                f"| {run.profile} cumulative | unavailable | unavailable | "
                "unavailable | no |"
            )
        for phase, value in sorted(totals.items()):
            total = value if isinstance(value, dict) else {}
            wall = total.get("wall_seconds")
            lines.append(
                f"| {run.profile} cumulative | {phase} | {_fmt(wall)} | "
                f"{_fmt(total.get('max_observed_process_rss_bytes'))} | "
                f"{'yes' if total.get('complete') else 'no'} |"
            )
    return lines + [""]


def _missing_metrics_section(runs: tuple[_SavedRun, ...]) -> list[str]:
    lines = ["## Missing metrics", ""]
    missing: dict[tuple[str, str, str, str], int] = defaultdict(int)
    fields = (
        "wall_seconds",
        "user_cpu_seconds",
        "system_cpu_seconds",
        "max_process_rss_bytes",
    )
    for run in runs:
        for command in run.commands:
            phase = str(command.get("phase", "unknown"))
            measurement = command.get("measurement")
            if not isinstance(measurement, dict):
                reason = (
                    "planned command"
                    if run.manifest["status"] == "planned"
                    else "command measurement absent"
                )
                for field in fields:
                    missing[run.profile, phase, field, reason] += 1
                continue
            reason = str(
                measurement.get(
                    "resource_method", "provider did not give a reason"
                )
            )
            failure = measurement.get("failure")
            if isinstance(failure, dict) and failure.get("message"):
                reason += f": {failure['message']}"
            for field in fields:
                if measurement.get(field) is None:
                    missing[run.profile, phase, field, reason] += 1
    if not missing:
        lines.append("No unavailable command metrics were recorded.")
    else:
        for (profile, phase, field, reason), count in sorted(missing.items()):
            lines.append(
                f"- `{profile}/{phase}` {field}: unavailable ({reason}); "
                f"{count} command(s)"
            )
    return lines + [""]


def _disk_section(runs: tuple[_SavedRun, ...]) -> list[str]:
    lines = [
        "## Memory scope and disk categories",
        "",
        "RSS is the maximum observed command high-water; not tree peak. It is "
        "not a simultaneous process-tree memory measurement.",
        "",
        "Disk observations retain logical and allocated bytes separately.",
        "",
        "| Profile/sample | Category | Logical bytes | Allocated bytes |",
        "|---|---|---:|---:|",
    ]
    for run in runs:
        for sample in run.samples:
            for category, raw in sorted(sample["disk"].items()):
                value = raw if isinstance(raw, dict) else {}
                lines.append(
                    f"| {run.profile}/{sample['id']} | {category} | "
                    f"{_fmt(value.get('logical_bytes'))} | "
                    f"{_fmt(value.get('allocated_bytes'))} |"
                )
    return lines + [""]


def _diagnostic_section(runs: tuple[_SavedRun, ...]) -> list[str]:
    lines = ["## Invocation diagnostics", ""]
    for run in runs:
        if not run.diagnostics:
            lines.append(
                f"- `{run.profile}`: unavailable (no saved diagnostics)"
            )
            continue
        for record in run.diagnostics:
            failures = record.get("failures", [])
            detail = "; ".join(map(str, failures)) if failures else "valid"
            summary = record.get("summary")
            evidence = ""
            if isinstance(summary, dict):
                parts = [f"counts={_canonical(summary.get('counts', {}))}"]
                for name in (
                    "queries",
                    "preprocess",
                    "bitcode_compilations",
                    "cache_hits",
                ):
                    parts.append(f"{name}={_canonical(summary.get(name))}")
                parts.append(
                    f"unobserved={_canonical(summary.get('unobserved', {}))}"
                )
                parts.append(f"scope={summary.get('scope', 'unavailable')}")
                evidence = "; " + "; ".join(parts)
            lines.append(
                f"- `{run.profile}/{record.get('phase', 'unknown')}`: "
                f"{detail}{evidence}"
            )
    return lines + [""]


def _coverage_section(runs: tuple[_SavedRun, ...]) -> list[str]:
    lines = ["## Coverage boundaries and limitations", ""]
    boundaries = set()
    limitations = set()
    for run in runs:
        fixture = run.manifest["fixture"]
        for target in fixture.get("targets", []):
            if isinstance(target, dict) and target.get("coverage_boundary"):
                boundaries.add(str(target["coverage_boundary"]))
        limitations.update(map(str, fixture.get("limitations", [])))
        limitations.update(map(str, run.manifest.get("limitations", [])))
    if not boundaries:
        boundaries.add(
            "project-owned compiled sources; external dependencies, runtime, "
            "prebuilt standard libraries, and assembly are outside coverage"
        )
    lines.extend(f"- Coverage: {value}" for value in sorted(boundaries))
    lines.extend(f"- Limitation: {value}" for value in sorted(limitations))
    return lines + [""]


def _csv(rows: list[dict]) -> str:
    output = io.StringIO(newline="")
    fieldnames = list(rows[0]) if rows else ["profile"]
    writer = csv.DictWriter(output, fieldnames=fieldnames, lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    return output.getvalue()


def _fmt(value: object) -> str:
    if value is None:
        return "unavailable"
    if isinstance(value, bool) or not isinstance(value, int | float):
        return str(value)
    number = float(value)
    if not math.isfinite(number):
        return "unavailable"
    return f"{number:.6f}".rstrip("0").rstrip(".") or "0"
