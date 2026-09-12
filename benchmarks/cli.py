"""Command-line interface for reproducible workflow benchmarks."""

import os
from dataclasses import replace
from pathlib import Path
from typing import Annotated, Any

import typer

from benchmarks.fixtures import FixtureError, PreparedFixture, prepare_fixture
from benchmarks.process import CommandFailed
from benchmarks.recipes import DEFAULT_PROFILES, get_recipe, recipes
from benchmarks.records import RecordError, read_json, write_json
from benchmarks.report import ReportError, generate_report
from benchmarks.runner import RunOptions, run_profile
from benchmarks.toolchains import (
    Toolchain,
    ToolchainError,
    resolve_dependencies,
)
from benchmarks.workspace import Workspace, WorkspaceError

app = typer.Typer(
    no_args_is_help=True,
    help="Prepare, run, and report rllvm workflow benchmarks.",
)


class CliError(RuntimeError):
    """Invalid command input caught at the user-facing boundary."""


ProfileOption = Annotated[
    list[str] | None,
    typer.Option(
        "--profile",
        "-p",
        help="Profile to select; repeat for more than one.",
    ),
]
OutputOption = Annotated[
    Path,
    typer.Option("--output", "-o", help="New output directory."),
]


@app.command("profiles")
def list_profiles() -> None:
    """List the registered benchmark profiles."""
    defaults = set(DEFAULT_PROFILES)
    for recipe in recipes():
        suffix = " (default)" if recipe.profile_id in defaults else ""
        typer.echo(
            f"{recipe.profile_id}{suffix}\t{recipe.project}/{recipe.build_system}"
        )


@app.command()
def prepare(
    examples_root: Annotated[
        Path,
        typer.Option(
            "--examples-root",
            help="Read-only root containing project source checkouts.",
        ),
    ],
    dependencies_root: Annotated[
        Path,
        typer.Option(
            "--dependencies-root",
            help="Root containing dependency prefixes named by dependency.",
        ),
    ],
    tool_root: Annotated[
        list[Path],
        typer.Option(
            "--tool-root",
            help="Tool or tool/bin root; repeat to search multiple roots.",
        ),
    ],
    output: OutputOption,
    profile: ProfileOption = None,
    all_profiles: Annotated[
        bool,
        typer.Option("--all", help="Prepare every registered profile."),
    ] = False,
) -> None:
    """Prepare private pinned source snapshots and resolved manifests."""
    try:
        selected = _select_recipes(profile, all_profiles)
        _require_new_output(output)
        _require_directory(examples_root, "examples root")
        for root in tool_root:
            _require_directory(root, "tool root")
        output = output.resolve()
        examples_root = examples_root.resolve()
        dependencies_root = dependencies_root.resolve()
        tool_root = [root.resolve() for root in tool_root]
        required_tools = tuple(
            dict.fromkeys(
                name for recipe in selected for name in recipe.required_tools
            )
        )
        tool_paths = _resolve_tools(required_tools, tuple(tool_root))
        dependencies = tuple(
            dict.fromkeys(
                name
                for recipe in selected
                for name in recipe.required_dependencies
            )
        )
        prefixes = _resolve_dependencies(dependencies, dependencies_root)
        workspace = Workspace.create(output)
        tools = Toolchain.discover(
            required_tools,
            workspace.root / "tool-discovery",
            paths=tool_paths,
        )
        if dependencies:
            tools = replace(
                tools,
                dependencies=resolve_dependencies(
                    tools,
                    dependencies,
                    prefixes,
                    workspace.root / "dependency-discovery",
                ),
            )
        manifests = []
        for recipe in selected:
            source = examples_root / recipe.project
            _require_directory(source, f"source checkout for {recipe.project}")
            prepared = prepare_fixture(recipe, source, workspace.root, tools)
            manifests.append(
                {
                    "profile": recipe.profile_id,
                    "identity": prepared.identity,
                    "manifest": str(prepared.manifest_path.absolute()),
                }
            )
        group = workspace.root / "prepared-group.json"
        write_json(
            group,
            {
                "schema_version": 1,
                "kind": "prepared-fixture-group",
                "profiles": [recipe.profile_id for recipe in selected],
                "manifests": manifests,
            },
        )
        typer.echo(str(group))
    except (
        CliError,
        FixtureError,
        CommandFailed,
        ToolchainError,
        WorkspaceError,
        OSError,
    ) as error:
        _fail(error, output)


@app.command()
def run(
    manifest: Annotated[
        list[Path],
        typer.Option(
            "--manifest",
            "-m",
            help="Prepared fixture or group manifest; repeat as needed.",
        ),
    ],
    output: OutputOption,
    profile: ProfileOption = None,
    all_profiles: Annotated[
        bool,
        typer.Option("--all", help="Use every profile in group manifests."),
    ] = False,
    repetitions: Annotated[
        int | None,
        typer.Option(help="Repeated benchmark blocks; baseline default is 3."),
    ] = None,
    jobs: Annotated[
        int,
        typer.Option(help="Serial workload build job count."),
    ] = 2,
    seed: Annotated[
        int,
        typer.Option(help="Seed for balanced arm ordering."),
    ] = 144,
    extraction_repeats: Annotated[
        int,
        typer.Option(help="Repeated all-target extraction validations."),
    ] = 2,
    smoke: Annotated[
        bool,
        typer.Option(
            "--smoke/--baseline",
            help="Use one correctness repetition or the baseline default.",
        ),
    ] = False,
    dry_run: Annotated[
        bool,
        typer.Option(
            help="Plan the runner's commands without executing them."
        ),
    ] = False,
    rllvm_provenance: Annotated[
        Path | None,
        typer.Option(
            help="Versioned JSON record with known rllvm build provenance."
        ),
    ] = None,
) -> None:
    """Run resolved prepared fixtures serially through the runner API."""
    repeat_count = (
        1
        if smoke and repetitions is None
        else 3
        if repetitions is None
        else repetitions
    )
    try:
        if repeat_count < 1 or jobs < 1 or extraction_repeats < 1:
            raise CliError(
                "repetitions, jobs, and extraction repeats must be positive"
            )
        if jobs > (os.cpu_count() or 1):
            raise CliError("jobs must not exceed the host logical CPU count")
        if smoke and repeat_count != 1:
            raise CliError("smoke mode requires exactly one repetition")
        _require_new_output(output)
        output = output.resolve()
        prepared = _load_prepared(tuple(manifest))
        prepared = _filter_prepared(prepared, profile, all_profiles)
        provenance = _load_provenance(rllvm_provenance)
        workspace = Workspace.create(output)
        runs = []
        unreached = []
        valid = True
        interrupted = False
        for index, fixture in enumerate(prepared):
            run_root = workspace.root / fixture.recipe.profile_id
            result = run_profile(
                fixture,
                fixture.toolchain,
                RunOptions(
                    run_root,
                    repetitions=repeat_count,
                    jobs=jobs,
                    seed=seed,
                    extraction_repeats=extraction_repeats,
                    dry_run=dry_run,
                    rllvm_provenance=provenance,
                ),
            )
            runs.append(
                {
                    "profile": fixture.recipe.profile_id,
                    "fixture_identity": fixture.identity,
                    "manifest": str((result.root / "run.json").absolute()),
                    "status": result.status,
                    "valid": result.valid,
                }
            )
            valid &= result.valid or dry_run and result.status == "planned"
            if result.status == "interrupted":
                interrupted = True
                unreached = [
                    {
                        "profile": remaining.recipe.profile_id,
                        "fixture_identity": remaining.identity,
                        "reason": "not reached: prior profile interrupted",
                    }
                    for remaining in prepared[index + 1 :]
                ]
                break
        group = workspace.root / "run-group.json"
        write_json(
            group,
            {
                "schema_version": 1,
                "kind": "workflow-run-group",
                "status": "interrupted"
                if interrupted
                else "planned"
                if dry_run and valid
                else "valid"
                if valid
                else "invalid",
                "options": {
                    "repetitions": repeat_count,
                    "jobs": jobs,
                    "seed": seed,
                    "extraction_repeats": extraction_repeats,
                    "dry_run": dry_run,
                },
                "runs": runs,
                "unreached": unreached,
            },
        )
        typer.echo(str(group))
        if interrupted:
            typer.echo(
                f"benchmark interrupted; retained records: {group}",
                err=True,
            )
            raise typer.Exit(130)
        if not valid:
            typer.echo(
                f"benchmark result is invalid; retained records: {group}",
                err=True,
            )
            raise typer.Exit(1)
    except typer.Exit:
        raise
    except (
        CliError,
        FixtureError,
        CommandFailed,
        ToolchainError,
        WorkspaceError,
        RecordError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        _fail(error, output)


@app.command()
def report(
    manifests: Annotated[
        list[Path],
        typer.Argument(help="Saved run, run.json, or run-group.json paths."),
    ],
    output: OutputOption,
) -> None:
    """Generate deterministic reports from saved records only."""
    try:
        artifacts = generate_report(tuple(manifests), output)
        typer.echo(str(artifacts.markdown))
        typer.echo(str(artifacts.csv))
        typer.echo(str(artifacts.json))
    except (ReportError, RecordError, OSError) as error:
        _fail(error, output)


def _select_recipes(profile: list[str] | None, all_profiles: bool):
    if all_profiles and profile:
        raise CliError("--all cannot be combined with --profile")
    names = (
        tuple(recipe.profile_id for recipe in recipes())
        if all_profiles
        else tuple(profile or DEFAULT_PROFILES)
    )
    try:
        return tuple(get_recipe(name) for name in dict.fromkeys(names))
    except ValueError as error:
        raise CliError(str(error)) from error


def _require_new_output(path: Path) -> None:
    if path.exists() or path.is_symlink():
        raise CliError(f"output already exists: {path}")


def _require_directory(path: Path, label: str) -> None:
    if not path.is_dir():
        raise CliError(f"{label} is missing: {path}")


def _resolve_tools(names: tuple[str, ...], roots: tuple[Path, ...]):
    if not roots:
        raise CliError("at least one explicit --tool-root is required")
    result = {}
    for name in names:
        matches = [
            candidate
            for root in roots
            for candidate in (root / name, root / "bin" / name)
            if candidate.is_file()
        ]
        if not matches:
            raise CliError(f"required tool not found in tool roots: {name}")
        result[name] = matches[0].absolute()
    return result


def _resolve_dependencies(names: tuple[str, ...], root: Path):
    if not names:
        return {}
    _require_directory(root, "dependencies root")
    result = {}
    for name in names:
        candidate = root / name
        if not candidate.is_dir() and root.name == name:
            candidate = root
        if not candidate.is_dir():
            raise CliError(f"dependency prefix is missing: {candidate}")
        result[name] = candidate.absolute()
    return result


def _load_prepared(paths: tuple[Path, ...]) -> tuple[PreparedFixture, ...]:
    if not paths:
        raise CliError("at least one --manifest is required")
    resolved = []
    for path in paths:
        value = read_json(path)
        kind = value.get("kind")
        if kind == "prepared-fixture":
            resolved.append(path)
        elif kind == "prepared-fixture-group":
            entries = value.get("manifests")
            if not isinstance(entries, list) or not entries:
                raise CliError(f"prepared group has no manifests: {path}")
            for entry in entries:
                if not isinstance(entry, dict) or not isinstance(
                    entry.get("manifest"), str
                ):
                    raise CliError(f"malformed prepared group entry: {path}")
                child = Path(entry["manifest"])
                resolved.append(
                    child if child.is_absolute() else path.parent / child
                )
        else:
            raise CliError(f"unsupported prepared manifest kind: {kind}")
    loaded = []
    for path in resolved:
        fixture = PreparedFixture.from_manifest(read_json(path))
        loaded.append(replace(fixture, source=fixture.source.resolve()))
    fixtures = tuple(loaded)
    names = [fixture.recipe.profile_id for fixture in fixtures]
    if len(names) != len(set(names)):
        raise CliError("prepared manifests contain duplicate profile ids")
    return fixtures


def _filter_prepared(
    fixtures: tuple[PreparedFixture, ...],
    profile: list[str] | None,
    all_profiles: bool,
) -> tuple[PreparedFixture, ...]:
    if all_profiles and profile:
        raise CliError("--all cannot be combined with --profile")
    if not profile:
        return fixtures
    requested = tuple(dict.fromkeys(profile))
    available = {fixture.recipe.profile_id: fixture for fixture in fixtures}
    missing = [name for name in requested if name not in available]
    if missing:
        raise CliError("unknown prepared profile: " + ", ".join(missing))
    return tuple(available[name] for name in requested)


def _load_provenance(path: Path | None) -> dict[str, str]:
    if path is None:
        return {}
    value = read_json(path)
    if value.get("kind") == "rllvm-provenance":
        value = value.get("provenance")
    if not isinstance(value, dict) or not all(
        isinstance(key, str) and isinstance(item, str)
        for key, item in value.items()
    ):
        raise CliError("rllvm provenance must be a JSON object of strings")
    return {str(key): str(item) for key, item in value.items()}


def _fail(error: BaseException, output: Path) -> Any:
    retained = f"; retained path: {output}" if output.exists() else ""
    typer.echo(f"error: {error}{retained}", err=True)
    raise typer.Exit(1)
