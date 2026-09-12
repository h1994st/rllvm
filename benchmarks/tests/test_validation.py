"""Validation must reject readable but incomplete captures."""

import shutil
import tempfile
from dataclasses import replace
from pathlib import Path

import pytest

from benchmarks.coverage import Coverage, cmake_coverage
from benchmarks.recipes import Target
from benchmarks.tests.validation_support import (
    FIXTURES,
    environment,
    run,
    tools_at,
)
from benchmarks.toolchains import Toolchain
from benchmarks.validation import (
    Extraction,
    validate_autotools_configuration,
    validate_behavior,
    validate_extraction_set,
    validate_target,
)


@pytest.fixture
def _validation_context(request, tmp_path: Path) -> None:
    root = tmp_path / "validation space"
    root.mkdir()
    tools = tools_at(root)
    env = environment(root, tools)
    source = root / "source"
    shutil.copytree(FIXTURES / "coverage", source)
    target = Target(
        "app",
        "app",
        "executable",
        ("project_",),
        (),
        ("main",),
        (("{build}/app",),),
        "app",
        ("*.c", "*.cpp"),
    )
    for mode in ("native", "wrapped"):
        build = root / mode
        query = build / ".cmake/api/v1/query/codemodel-v2"
        query.parent.mkdir(parents=True)
        query.touch()
        run(
            (
                tools.path("cmake"),
                "-S",
                source,
                "-B",
                build,
                "-G",
                "Ninja",
                "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON",
                "-DCMAKE_BUILD_TYPE=Debug",
                "-DCMAKE_C_COMPILER="
                + tools.path("rllvm-cc" if mode == "wrapped" else "clang"),
                "-DCMAKE_CXX_COMPILER="
                + tools.path("rllvm-cxx" if mode == "wrapped" else "clang++"),
            ),
            root,
            env,
            mode + "-configure",
        )
        run(
            (tools.path("cmake"), "--build", build, "-j2"),
            root,
            env,
            mode + "-build",
        )
    output = root / "app.bc"
    run(
        (
            tools.path("rllvm-get-bc"),
            "--save-manifest",
            "-o",
            output,
            root / "wrapped/app",
        ),
        root,
        env,
        "extract",
    )
    instance = request.instance
    instance.root = root
    instance.tools = tools
    instance.env = env
    instance.source = source
    instance.target = target
    instance.output = output
    instance.extraction = Extraction(
        output,
        root / "wrapped/app.bc.manifest",
    )
    instance.coverage = cmake_coverage(target, source, root / "wrapped")


@pytest.mark.usefixtures("_validation_context")
class TestValidation:
    root: Path
    tools: Toolchain
    env: dict[str, str]
    source: Path
    target: Target
    output: Path
    extraction: Extraction
    coverage: Coverage

    def validate(self, **kwargs):
        directory = Path(tempfile.mkdtemp(dir=self.root))
        return validate_target(
            self.target,
            self.root / "native/app",
            self.root / "wrapped/app",
            kwargs.pop("extracted", self.extraction),
            self.tools,
            logs=directory,
            env=self.env,
            coverage=self.coverage,
            **kwargs,
        )

    def test_real_cpp_capture_and_direct_object_sources(self):
        result = self.validate()
        assert result.valid, result.failures
        assert {Path(s.path).name for s in self.coverage.required} == {
            "main.cpp",
            "first.c",
            "second.c",
        }
        assert any("unused" in x.reason for x in self.coverage.exclusions)
        assert any("shared" in x.reason for x in self.coverage.exclusions)
        assert result.module_count == 3

    def test_missing_recorded_module_rejects_present_extraction(self):
        module = Path(self.extraction.manifest.read_text().splitlines()[0])
        saved = module.read_bytes()
        module.unlink()
        try:
            result = self.validate()
            assert not result.valid
            assert "missing recorded module" in " ".join(result.failures)
        finally:
            module.write_bytes(saved)

    def test_readable_ir_missing_project_definition_is_rejected(self):
        source = self.root / "incomplete.c"
        source.write_text(
            "int project_first(void){return 17;} int main(){return 0;}"
        )
        output = self.root / "incomplete.bc"
        run(
            (
                self.tools.path("clang"),
                "-emit-llvm",
                "-c",
                source,
                "-o",
                output,
            ),
            self.root,
            self.env,
            "incomplete",
        )
        result = self.validate(
            extracted=replace(self.extraction, output=output)
        )
        assert not result.valid
        assert "project_second" in " ".join(result.failures)

    def test_missing_direct_source_rejected_even_with_all_symbols(self):
        manifest = self.root / "incomplete.manifest"
        paths = self.extraction.manifest.read_text().splitlines()
        manifest.write_text("\n".join(paths[1:]))
        result = self.validate(
            extracted=replace(self.extraction, manifest=manifest)
        )
        assert not result.valid
        assert "source" in " ".join(result.failures)

    def test_missing_target_and_changed_repeat_are_rejected(self):
        result = self.validate()
        absent = validate_extraction_set((self.target,), {})
        assert not absent.valid
        good = validate_extraction_set(
            (self.target,), {"app": result}, repeated={"app": result}
        )
        assert good.valid, good.failures
        assert good.union_module_count == 3
        changed = replace(result, ir_definitions=())
        bad = validate_extraction_set(
            (self.target,), {"app": result}, repeated={"app": changed}
        )
        assert not bad.valid

    def test_behavior_and_incremental_probe_require_actual_output(self):
        native = run(
            (self.root / "native/app",), self.root, self.env, "behavior-n"
        )
        wrapped = run(
            (self.root / "wrapped/app",), self.root, self.env, "behavior-w"
        )
        assert validate_behavior((native,), (wrapped,)).valid
        assert not validate_behavior(
            (native,), (wrapped,), expected_suffix="-rllvm-benchmark"
        ).valid

    def test_empty_or_nonnumeric_autotools_limit_rejects_success(self):
        for value in ("", "unlimited", "0", "-1"):
            directory = Path(tempfile.mkdtemp(dir=self.root))
            (directory / "libtool").write_text(f'max_cmd_len="{value}"\n')
            assert not validate_autotools_configuration(directory).valid
        directory = Path(tempfile.mkdtemp(dir=self.root))
        (directory / "libtool").write_text('max_cmd_len="262144"\n')
        assert validate_autotools_configuration(directory).valid

    def test_missing_independent_build_evidence_is_not_success(self):
        (self.root / "empty").mkdir(exist_ok=True)
        coverage = cmake_coverage(
            self.target, self.source, self.root / "empty"
        )
        assert not coverage.complete
        assert coverage.failures

    def test_real_repeated_extraction_has_identical_ir_and_modules(self):
        output = self.root / "repeated.bc"
        record = run(
            (
                self.tools.path("rllvm-get-bc"),
                "--save-manifest",
                "-o",
                output,
                self.root / "wrapped/app",
            ),
            self.root,
            self.env,
            "repeat",
        )
        original = self.validate()
        repeated = self.validate(
            extracted=Extraction(
                output, self.root / "wrapped/repeated.bc.manifest", record
            )
        )
        result = validate_extraction_set(
            (self.target,), {"app": original}, repeated={"app": repeated}
        )
        assert result.valid, result.failures
        changed = replace(repeated, ir_sha256="changed body with same symbols")
        assert not validate_extraction_set(
            (self.target,), {"app": original}, repeated={"app": changed}
        ).valid

    def test_repeat_with_wrong_target_identity_is_rejected(self):
        original = self.validate()
        wrong = replace(original, target_id="another-target")
        result = validate_extraction_set(
            (self.target,), {"app": original}, repeated={"app": wrong}
        )
        assert not result.valid, result


class TestCargoValidation:
    def test_matched_cargo_capture_uses_raw_project_definitions(self):
        from benchmarks.coverage import cargo_coverage

        with tempfile.TemporaryDirectory(
            prefix="cargo validation "
        ) as directory:
            root = Path(directory)
            tools = tools_at(root)
            env = environment(root, tools)
            source = root / "source"
            shutil.copytree(FIXTURES / "cargo", source)
            target = Target(
                "static",
                "release/libcoverage_project.a",
                "static",
                ("project_",),
                ("coverage_project",),
                ("project_first",),
                (),
                "coverage_project",
                ("src/*.rs",),
            )
            evidence = []
            commands = []
            from benchmarks.process import Command
            from benchmarks.recipes import get_recipe
            from benchmarks.validation import validate_configuration

            for mode in ("native", "wrapped"):
                build = root / mode
                cargo_env = dict(
                    env,
                    CARGO_TARGET_DIR=str(build),
                    CARGO_INCREMENTAL="0",
                    RUSTC=tools.path("rustc"),
                    RUSTC_WRAPPER=(
                        tools.path("rllvm-rustc") if mode == "wrapped" else ""
                    ),
                    RUSTC_WORKSPACE_WRAPPER="",
                    RUSTFLAGS="-Cdebuginfo=2",
                )
                command = (
                    tools.path("cargo"),
                    "build",
                    "--offline",
                    "--release",
                    "-j2",
                    "-vv",
                    "--manifest-path",
                    source / "Cargo.toml",
                    "--message-format=json-render-diagnostics",
                    "--config",
                    "profile.release.codegen-units=1",
                    "--config",
                    "profile.release.build-override.codegen-units=1",
                )
                evidence.append(run(command, root, cargo_env, mode))
                commands.append(
                    Command(tuple(map(str, command)), root, cargo_env)
                )
            configuration = validate_configuration(
                get_recipe("quiche-cargo"),
                (commands[0],),
                (commands[1],),
                tools,
                root / "native",
                root / "wrapped",
                native_records=(evidence[0],),
                wrapped_records=(evidence[1],),
            )
            assert configuration.valid, configuration.failures
            native_log = Path(evidence[0].stderr)
            original_log = native_log.read_text()
            assert "--crate-name build_script_build" in original_log
            native_log.write_text(
                original_log.replace("codegen-units=1", "codegen-units=16", 1)
            )
            mismatched = validate_configuration(
                get_recipe("quiche-cargo"),
                (commands[0],),
                (commands[1],),
                tools,
                root / "native",
                root / "wrapped",
                native_records=(evidence[0],),
                wrapped_records=(evidence[1],),
            )
            assert not mismatched.valid
            native_log.write_text(original_log)
            output = root / "cargo.bc"
            wrapped = root / "wrapped" / target.artifact
            extraction = run(
                (
                    tools.path("rllvm-get-bc"),
                    "--save-manifest",
                    "-o",
                    output,
                    wrapped,
                ),
                root,
                env,
                "extract",
            )
            coverage = cargo_coverage(
                target,
                source,
                root / "wrapped",
                (evidence[1],),
                project_package="coverage_project",
            )
            boundaries = {
                item.category: item.count for item in coverage.exclusions
            }
            assert boundaries["dependency-crates"] == 0
            assert boundaries["host-build-tools"] == 1
            result = validate_target(
                target,
                root / "native" / target.artifact,
                wrapped,
                Extraction(
                    output, wrapped.parent / "cargo.bc.manifest", extraction
                ),
                tools,
                logs=root / "validation",
                env=env,
                coverage=coverage,
            )
            assert result.valid, result.failures
            assert result.native_definitions == result.wrapped_definitions
            assert any(
                "coverage_project" in s for s in result.native_definitions
            )


class TestConfiguration:
    def test_changed_compilation_flag_is_rejected(self):
        from benchmarks.process import Command
        from benchmarks.recipes import get_recipe
        from benchmarks.validation import validate_configuration

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = tools_at(root)
            env = environment(root, tools)
            recipe = get_recipe("nghttp2-c-cmake")
            native = Command(
                (tools.path("clang"), "-O2"), root / "native", env
            )
            wrapped = Command(
                (tools.path("rllvm-cc"), "-O3"), root / "wrapped", env
            )
            result = validate_configuration(
                recipe,
                (native,),
                (wrapped,),
                tools,
                root / "native",
                root / "wrapped",
            )
            assert not result.valid
            matching = replace(wrapped, argv=(tools.path("rllvm-cc"), "-O2"))
            assert validate_configuration(
                recipe,
                (native,),
                (matching,),
                tools,
                root / "native",
                root / "wrapped",
            ).valid

    def test_missing_compiler_selection_does_not_establish_parity(self):
        from benchmarks.process import Command
        from benchmarks.recipes import get_recipe
        from benchmarks.validation import validate_configuration

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = tools_at(root)
            env = environment(root, tools)
            Path(env["RLLVM_CONFIG"]).write_text("")
            native = Command(
                (tools.path("clang"), "-c", "source.c"), root / "native", env
            )
            wrapped = Command(
                (tools.path("rllvm-cc"), "-c", "source.c"),
                root / "wrapped",
                env,
            )
            result = validate_configuration(
                get_recipe("nghttp2-c-cmake"),
                (native,),
                (wrapped,),
                tools,
                root / "native",
                root / "wrapped",
            )
            assert not result.valid, result

            # Keep C explicit while removing only the C++ selector; toolchain
            # discovery alone cannot establish the wrapper's C++ selection.
            environment(root, tools)
            config = Path(env["RLLVM_CONFIG"])
            config.write_text(
                "\n".join(
                    line
                    for line in config.read_text().splitlines()
                    if not line.startswith("clangxx_filepath")
                )
            )
            cxx_result = validate_configuration(
                get_recipe("nghttp2-cxx-cmake"),
                (native,),
                (wrapped,),
                tools,
                root / "native",
                root / "wrapped",
            )
            assert not cxx_result.valid, cxx_result
            assert "clangxx_filepath" in " ".join(cxx_result.failures)


class TestBuildEvidence:
    def test_verbose_make_follows_direct_objects_without_unused_archive(self):
        from benchmarks.coverage import autotools_coverage

        with tempfile.TemporaryDirectory(prefix="make evidence ") as directory:
            root = Path(directory)
            tools = tools_at(root)
            env = environment(root, tools)
            source = root / "source"
            source.mkdir()
            (source / "main.c").write_text("int main(void){return 0;}")
            (source / "unused.c").write_text(
                "int project_unused(void){return 2;}"
            )
            build = root / "build"
            build.mkdir()
            (build / "Makefile").write_text(
                "all: app\n"
                'main.o:\n\t"'
                + tools.path("clang")
                + '" -c "'
                + str(source / "main.c")
                + '" -o main.o\n'
                'unused.o:\n\t"'
                + tools.path("clang")
                + '" -c "'
                + str(source / "unused.c")
                + '" -o unused.o\n'
                'libunused.a: unused.o\n\t"'
                + tools.path("llvm-ar")
                + '" rcs libunused.a unused.o\n'
                'app: main.o libunused.a\n\t"'
                + tools.path("clang")
                + '" main.o libunused.a -o app\n'
            )
            record = run(("make", "-C", build, "-j2"), root, env, "build")
            target = Target(
                "app",
                "app",
                "executable",
                (),
                (),
                ("main",),
                (),
                "app",
                ("*.c",),
            )
            coverage = autotools_coverage(target, source, build, (record,))
            assert coverage.complete, coverage.failures
            assert tuple(Path(s.path).name for s in coverage.required) == (
                "main.c",
            )
            assert coverage.exclusions[0].count == 1

    def test_munit_behavior_compares_results_but_retains_timing_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outputs = (
                "Running test suite with seed 0x123...\n/a [ OK ] [ 0.1 / 0.2 CPU ]\n1 of 1 (100%) tests successful, 0 (0%) test skipped.\n",
                "Running test suite with seed 0x456...\n/a [ OK ] [ 0.3 / 0.4 CPU ]\n1 of 1 (100%) tests successful, 0 (0%) test skipped.\n",
            )
            import sys

            from benchmarks.process import Command, checked

            observations = tuple(
                checked(
                    Command(
                        (
                            sys.executable,
                            "-c",
                            "import sys;sys.stdout.write(sys.argv[1])",
                            output,
                        ),
                        root,
                        {},
                    ),
                    root,
                    str(i),
                )
                for i, output in enumerate(outputs)
            )
            assert validate_behavior(
                (observations[0],), (observations[1],), kind="munit"
            ).valid
            Path(observations[1].stdout).write_text(
                outputs[1].replace("[ OK ]", "[ FAIL ]")
            )
            assert not validate_behavior(
                (observations[0],), (observations[1],), kind="munit"
            ).valid


class TestSymbolReader:
    def test_gnu_unique_definitions_are_not_undefined_references(self):
        from benchmarks.validation import _defined_symbols

        assert _defined_symbols(
            "archive(member.o):\nproject_unique u 0 8\nproject_call T 8 c\nexternal U 0 0\n"
        ) == {"project_unique", "project_call"}
