"""Recipe commands cross real CMake, compiler, and Cargo boundaries."""

import os
import subprocess
from dataclasses import replace
from pathlib import Path

import pytest

from benchmarks.process import checked
from benchmarks.recipes import get_recipe
from benchmarks.tests.validation_support import llvm_tool_paths
from benchmarks.toolchains import Toolchain, ToolchainError, child_environment


class TestToolchain:
    def test_unsupported_host_and_missing_tool_fail_actionably(
        self, tmp_path: Path
    ):
        with pytest.raises(ToolchainError, match="macOS and Linux"):
            Toolchain.discover((), tmp_path, host="win32")
        with pytest.raises(ToolchainError, match="not found"):
            Toolchain.discover(("absent-rllvm-tool-144",), tmp_path)

    def test_allowlist_drops_credentials_and_ambient_compiler_settings(self):
        env = child_environment(
            {
                "PATH": "/bin",
                "HOME": "/tmp/home",
                "AWS_SECRET_ACCESS_KEY": "x",
                "CFLAGS": "-flto",
                "RUSTC_WRAPPER": "ambient-wrapper",
                "CARGO_ENCODED_RUSTFLAGS": "-Copt-level=0",
                "CC": "gcc",
            }
        )
        command = ("/bin/sh", "-c", "/usr/bin/env")
        output = subprocess.check_output(command, env=env, text=True)
        for forbidden in ("AWS_SECRET", "CFLAGS=", "RUSTC_WRAPPER=", "CC="):
            assert forbidden not in output

    @pytest.mark.full
    def test_rustup_and_cxx_symlinks_keep_driver_dispatch(
        self, tmp_path: Path
    ):
        tools = Toolchain.discover(
            ("clang++", "rustc"),
            tmp_path / "logs",
            paths=llvm_tool_paths(),
        )
        source = tmp_path / "main.cpp"
        source.write_text("#include <iostream>\nint main(){std::cout<<42;}")
        subprocess.run(
            (tools.path("clang++"), str(source), "-o", str(tmp_path / "app")),
            check=True,
            capture_output=True,
        )
        assert subprocess.check_output((tmp_path / "app",)) == b"42"
        assert "LLVM version:" in tools.tools["rustc"].version

    def test_incompatible_llvm_reader_is_rejected(self, tmp_path: Path):
        for name, version in (
            ("rustc", "rustc 1.98\nhost: test\nLLVM version: 22.1.8"),
            ("llvm-dis", "LLVM version 21.1.0"),
        ):
            script = tmp_path / name
            script.write_text(f"#!/bin/sh\nprintf '%s\\n' '{version}'\n")
            script.chmod(0o755)
        with pytest.raises(ToolchainError, match="LLVM.*22.*21"):
            Toolchain.discover(
                ("rustc", "llvm-dis"),
                tmp_path / "logs",
                paths={
                    name: tmp_path / name for name in ("rustc", "llvm-dis")
                },
            )


@pytest.mark.full
class TestRecipe:
    @pytest.fixture(autouse=True)
    def _recipe_context(self, tmp_path: Path) -> None:
        self.root = tmp_path / "recipe space"
        self.root.mkdir()
        self.source = self.root / "source space"
        self.source.mkdir()
        self.tools = Toolchain.discover(
            (
                "clang",
                "clang++",
                "llvm-ar",
                "llvm-ranlib",
                "cmake",
                "ninja",
                "cargo",
                "rustc",
            ),
            self.root / "tool logs",
            paths=llvm_tool_paths(),
        )
        self.env = child_environment(os.environ)
        self.env["RLLVM_CONFIG"] = str(self.root / "rllvm.toml")

    def test_cmake_build_with_spaces_preserves_initial_archive_tools(self):
        (self.source / "CMakeLists.txt").write_text(
            "cmake_minimum_required(VERSION 3.20)\nproject(tiny C CXX)\n"
            "add_library(nghttp2 SHARED value.c)\n"
            "add_library(nghttp2_static STATIC value.c)\n"
            "add_executable(main main.cpp)\n"
            "target_link_libraries(main PRIVATE nghttp2_static)\n"
        )
        (self.source / "value.c").write_text("int value(void){return 42;}\n")
        (self.source / "main.cpp").write_text(
            '#include <iostream>\nextern "C" int value(void);\n'
            "int main(){std::cout << value();}\n"
        )
        build = self.root / "build space"
        build.mkdir()
        recipe = get_recipe("nghttp2-c-cmake")
        for i, command in enumerate(
            recipe.configure_commands(
                self.source, build, self.tools, jobs=2, env=self.env
            )
            + recipe.build_commands(
                self.source, build, self.tools, jobs=2, env=self.env
            )
        ):
            checked(command, self.root / "logs", str(i))
        assert subprocess.check_output((build / "main",)) == b"42"
        cache = (build / "CMakeCache.txt").read_text()
        assert f"CMAKE_AR:FILEPATH={self.tools.path('llvm-ar')}" in cache
        assert "CMAKE_BUILD_TYPE:STRING=RelWithDebInfo" in cache

    def test_cargo_target_and_host_work_have_explicit_matched_codegen_units(
        self,
    ):
        (self.source / "Cargo.toml").write_text(
            '[package]\nname="quiche"\nversion="0.1.0"\nedition="2024"\n'
            '[features]\nffi=[]\n[build-dependencies]\nhelper={path="helper"}\n'
        )
        (self.source / "src").mkdir()
        (self.source / "src/lib.rs").write_text(
            'unsafe extern "C" {fn native_value()->u8;}\n'
            "pub fn value()->u8{unsafe{native_value()}}\n"
        )
        (self.source / "examples").mkdir()
        (self.source / "examples/client.rs").write_text(
            'fn main(){println!("{}",quiche::value());}\n'
        )
        (self.source / "native.c").write_text(
            "int native_value(void){return 42;}\n"
        )
        (
            self.source / "build.rs"
        ).write_text(r"""use std::{env, process::Command};
fn main() {
    helper::build();
    let out = env::var("OUT_DIR").unwrap();
    let object = format!("{out}/native.o");
    let library = format!("{out}/libnative.a");
    assert!(Command::new(env::var("CC").unwrap())
        .args(["-c", "native.c", "-o", &object]).status().unwrap().success());
    assert!(Command::new(env::var("AR").unwrap())
        .args(["rc", &library, &object]).status().unwrap().success());
    assert!(Command::new(env::var("RANLIB").unwrap())
        .arg(&library).status().unwrap().success());
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=native");
}
""")
        (self.source / "helper/src").mkdir(parents=True)
        (self.source / "helper/Cargo.toml").write_text(
            '[package]\nname="helper"\nversion="0.1.0"\nedition="2024"\n'
        )
        (self.source / "helper/src/lib.rs").write_text("pub fn build(){}\n")
        subprocess.run(
            (self.tools.path("cargo"), "generate-lockfile", "--offline"),
            cwd=self.source,
            check=True,
            capture_output=True,
        )
        recipe = get_recipe("quiche-cargo")
        # Transparent wrappers make the Cargo arm execute its native dependency
        # environment, without requiring the rllvm binaries in unit tests.
        wrappers = {}
        for name, real in (
            ("rllvm-cc", "clang"),
            ("rllvm-cxx", "clang++"),
            ("rllvm-rustc", "rustc"),
        ):
            script = self.root / name
            body = (
                'exec "$@"'
                if name == "rllvm-rustc"
                else f'exec "{self.tools.path(real)}" "$@"'
            )
            script.write_text(f"#!/bin/sh\n{body}\n")
            script.chmod(0o755)
            wrappers[name] = replace(self.tools.tools[real], path=str(script))
        tools = replace(self.tools, tools={**self.tools.tools, **wrappers})
        for wrapped in (False, True):
            build = self.root / f"cargo-{wrapped}"
            build.mkdir()
            (command,) = recipe.build_commands(
                self.source,
                build,
                tools,
                jobs=2,
                env=self.env,
                wrapped=wrapped,
            )
            result = checked(command, self.root / "logs", f"cargo-{wrapped}")
            invocations = [
                line
                for line in Path(result.stderr).read_text().splitlines()
                if "Running `" in line and "--crate-name" in line
            ]
            assert len(invocations) >= 4
            assert all("codegen-units=1" in x for x in invocations)
            assert any(
                "--crate-name build_script_build" in x for x in invocations
            )
            assert (
                subprocess.check_output((build / "release/examples/client",))
                == b"42\n"
            )

    def test_semantic_edit_changes_compiled_version_and_restores_exact_bytes(
        self,
    ):
        recipe = get_recipe("nghttp2-c-cmake")
        path = self.source / recipe.edit.path
        path.parent.mkdir(parents=True)
        original = (
            '#define NGHTTP2_VERSION "1.0"\n'
            'const char *version = NGHTTP2_VERSION, *other = "";\n'
        )
        path.write_text(original)
        edit = recipe.edit.apply(self.source)
        main = self.source / "main.c"
        main.write_text(
            "#include <stdio.h>\nextern const char *version;\n"
            "int main(){puts(version);}\n"
        )
        subprocess.run(
            (
                self.tools.path("clang"),
                str(path),
                str(main),
                "-o",
                str(self.root / "app"),
            ),
            check=True,
            capture_output=True,
        )
        assert (
            subprocess.check_output((self.root / "app",))
            == b"1.0-rllvm-benchmark\n"
        )
        assert edit.before_sha256 != edit.after_sha256
        assert "+" in edit.patch
        recipe.edit.restore(self.source, edit)
        assert path.read_text() == original

    def test_autotools_commands_preserve_compiler_paths_with_spaces(self):
        tools_directory = self.root / "compiler tools"
        tools_directory.mkdir()
        tools = dict(self.tools.tools)
        for name in ("clang", "clang++", "llvm-ar", "llvm-ranlib"):
            alias = tools_directory / name
            alias.symlink_to(self.tools.path(name))
            tools[name] = replace(tools[name], path=str(alias))
        # This small configure script exercises Autoconf's CC shell-command
        # convention, then a real make-driven compile/archive/ranlib sequence.
        configure = self.source / "configure"
        configure.write_text(
            "#!/bin/sh\nset -eu\n"
            "mkdir -p lib tests\n"
            'printf "CC = %s\\nAR = %s\\nRANLIB = %s\\n" '
            '"$CC" "$AR" "$RANLIB" > lib/Makefile\n'
            'printf "all:\\n\\t\\$(CC) -c %s/value.c -o value.o\\n'
            "\\t\\$(AR) rc libvalue.a value.o\\n"
            '\\t\\$(RANLIB) libvalue.a\\n" "$(dirname "$0")" '
            ">> lib/Makefile\n"
            'printf "main:\\n\\ttrue\\n" > tests/Makefile\n'
        )
        # Quote the source filename in the generated recipe too.
        configure.write_text(
            configure.read_text().replace(
                "-c %s/value.c", '-c \\"%s/value.c\\"'
            )
        )
        configure.chmod(0o755)
        (self.source / "value.c").write_text("int value(void){return 42;}\n")
        build = self.root / "autotools build"
        build.mkdir()
        make = Toolchain.discover(("make",), self.root / "make logs")
        toolchain = replace(self.tools, tools={**tools, **make.tools})
        recipe = get_recipe("nghttp2-c-autotools")
        commands = recipe.configure_commands(
            self.source, build, toolchain, jobs=2, env=self.env
        ) + recipe.build_commands(
            self.source, build, toolchain, jobs=2, env=self.env
        )
        for i, command in enumerate(commands):
            checked(command, self.root / "logs", f"auto-{i}")
        members = subprocess.check_output(
            (self.tools.path("llvm-ar"), "t", build / "lib/libvalue.a"),
            text=True,
        )
        assert members.strip() == "value.o"

    def test_cmake_evidence_contains_direct_sources_and_link_dependencies(
        self,
    ):
        (self.source / "CMakeLists.txt").write_text(
            "cmake_minimum_required(VERSION 3.20)\nproject(tiny C)\n"
            "add_library(nghttp2 SHARED value.c)\n"
            "add_library(nghttp2_static STATIC value.c)\n"
            "add_executable(main main.c)\n"
            "target_link_libraries(main PRIVATE nghttp2_static)\n"
        )
        (self.source / "value.c").write_text("int value(void){return 42;}\n")
        (self.source / "main.c").write_text("int main(void){return 0;}\n")
        build = self.root / "evidence build"
        recipe = get_recipe("nghttp2-c-cmake")
        evidence = recipe.prepare_build_evidence(build)
        (command,) = recipe.configure_commands(
            self.source, build, self.tools, jobs=2, env=self.env
        )
        checked(command, self.root / "logs", "evidence-configure")
        import json

        main = next(Path(evidence["codemodel"]).glob("target-main-*.json"))
        target = json.loads(main.read_text())
        assert [s["path"] for s in target["sources"]] == ["main.c"]
        assert target["dependencies"]
        assert Path(evidence["compile_commands"]).is_file()

    def test_dependency_identity_requires_library_and_version_evidence(self):
        from benchmarks.toolchains import resolve_dependencies

        prefix = self.root / "native dependency"
        (prefix / "include").mkdir(parents=True)
        (prefix / "lib").mkdir()
        (prefix / "include/ev.h").write_text(
            "#define EV_VERSION_MAJOR 4\n#define EV_VERSION_MINOR 33\n"
        )
        (self.source / "ev.c").write_text(
            "int ev_version_major(){return 4;}\n"
        )
        library = prefix / "lib/libev.dylib"
        subprocess.run(
            (
                self.tools.path("clang"),
                "-shared",
                str(self.source / "ev.c"),
                "-o",
                str(library),
            ),
            check=True,
            capture_output=True,
        )
        (dependency,) = resolve_dependencies(
            self.tools, ("libev",), {"libev": prefix}, self.root / "dep logs"
        )
        assert dependency.version == "4.33"
        assert str(library.resolve()) in dependency.libraries
        library.unlink()
        with pytest.raises(ToolchainError, match="missing libev library"):
            resolve_dependencies(
                self.tools,
                ("libev",),
                {"libev": prefix},
                self.root / "dep retry logs",
            )

    @pytest.mark.skipif(
        os.uname().sysname != "Darwin", reason="Darwin install names"
    )
    def test_version_probe_loads_uninstalled_autotools_library(self):
        recipe = get_recipe("nghttp2-c-autotools")
        build = self.root / "probe build"
        library = build / "lib/.libs/libnghttp2.dylib"
        library.parent.mkdir(parents=True)
        include = self.source / "lib/includes/nghttp2/nghttp2.h"
        include.parent.mkdir(parents=True)
        include.write_text(
            "struct info {const char *version_str;};\n"
            "const struct info *nghttp2_version(int);\n"
        )
        implementation = self.source / "library.c"
        implementation.write_text(
            "#include <nghttp2/nghttp2.h>\n"
            "const struct info *nghttp2_version(int n){\n"
            'static struct info v={"1.0"};return &v;}\n'
        )
        subprocess.run(
            (
                self.tools.path("clang"),
                "-shared",
                "-I" + str(include.parents[1]),
                str(implementation),
                "-Wl,-install_name,/nonexistent/libnghttp2.999.dylib",
                "-o",
                str(library),
            ),
            check=True,
            capture_output=True,
        )
        probe = self.root / "version.c"
        probe.write_text(recipe.version_probe_source())
        for i, command in enumerate(
            recipe.version_probe_commands(
                self.source,
                build,
                probe,
                self.root / "probe",
                self.tools,
                env=self.env,
            )
        ):
            result = checked(command, self.root / "logs", f"probe-{i}")
        assert Path(result.stdout).read_text() == "1.0\n"
