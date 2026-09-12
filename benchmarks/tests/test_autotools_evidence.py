"""Real make output boundaries found while integrating the serial runner."""

import shlex
from pathlib import Path

import pytest

from benchmarks.coverage import autotools_coverage
from benchmarks.process import Command, checked
from benchmarks.recipes import Target
from benchmarks.tests.validation_support import llvm_tool_paths
from benchmarks.toolchains import child_environment


@pytest.fixture
def make_fixture(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "main.c").write_text("int main(void){return 0;}\n")
    (source / "other.c").write_text("int other(void){return 1;}\n")
    build = tmp_path / "build"
    build.mkdir()
    clang = shlex.quote(str(llvm_tool_paths()["clang"]))
    target = Target(
        "app", "app", "executable", (), (), ("main",), (), "app", ("*.c",)
    )
    return source, build, clang, target


def test_configure_atfile_message_and_automake_continuation_are_not_lost(
    make_fixture,
):
    source, build, clang, target = make_fixture
    (build / "Makefile").write_text(
        "all: app\nmain.o:\n"
        "\t@echo 'checking for archiver @FILE support... @'\n"
        "\tdepbase=`echo main.o | sed 's|[^/]*$$|.deps/&|;s|\\.o$$||'`;\\\n"
        f"\t{clang} -c -o main.o {shlex.quote(str(source / 'main.c'))} &&\\\n"
        "\ttrue\napp: main.o\n"
        f"\t{clang} main.o -o app\n"
    )
    record = checked(
        Command(
            ("make", "--print-directory", "-j2"), build, child_environment()
        ),
        build / "logs",
        "make",
    )
    coverage = autotools_coverage(target, source, build, (record,))
    assert coverage.complete, coverage.failures
    assert [Path(s.path).name for s in coverage.required] == ["main.c"]


def test_recursive_make_leaving_restores_parent_cwd(make_fixture):
    source, build, clang, target = make_fixture
    (build / "nested").mkdir()
    (build / "nested/Makefile").write_text(
        f"all:\n\t{clang} -c {shlex.quote(str(source / 'other.c'))} -o other.o\n"
    )
    (build / "Makefile").write_text(
        "all:\n\t$(MAKE) --print-directory -C nested\n"
        f"\t{clang} -c {shlex.quote(str(source / 'main.c'))} -o main.o\n"
        f"\t{clang} main.o -o app\n"
    )
    record = checked(
        Command(
            ("make", "--print-directory", "-j2"), build, child_environment()
        ),
        build / "logs",
        "make",
    )
    coverage = autotools_coverage(target, source, build, (record,))
    assert coverage.complete, coverage.failures
    assert tuple(Path(s.path).name for s in coverage.required) == ("main.c",)


def test_real_compiler_response_remains_unsupported(make_fixture):
    source, build, clang, target = make_fixture
    (build / "input.rsp").write_text(shlex.quote(str(source / "main.c")))
    (build / "Makefile").write_text(
        f"all:\n\t{clang} @input.rsp -c -o main.o\n\t{clang} main.o -o app\n"
    )
    record = checked(
        Command(("make", "-j2"), build, child_environment()),
        build / "logs",
        "make",
    )
    coverage = autotools_coverage(target, source, build, (record,))
    assert not coverage.complete
    assert any("response-file" in f for f in coverage.failures)
