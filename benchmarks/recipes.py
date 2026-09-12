"""Pinned corpus and narrow, side-effect-free build command adapters."""

import difflib
import hashlib
import os
import shlex
from collections.abc import Mapping
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Literal

from benchmarks.process import Command
from benchmarks.toolchains import Toolchain, ToolchainError, sha256

BuildSystem = Literal["cmake", "autotools", "cargo"]
BENCHMARK_SUFFIX = "-rllvm-benchmark"


@dataclass(frozen=True)
class EditRecord:
    path: str
    before_sha256: str
    after_sha256: str
    patch: str


@dataclass(frozen=True)
class Edit:
    path: str
    before: str
    after: str
    expected_suffix: str = BENCHMARK_SUFFIX

    def apply(self, source: Path) -> EditRecord:
        path = source / self.path
        original = path.read_text()
        if original.count(self.before) != 1:
            raise ValueError(f"edit requires exactly one match in {self.path}")
        changed = original.replace(self.before, self.after, 1)
        record = EditRecord(
            self.path,
            sha256(path),
            hashlib.sha256(changed.encode()).hexdigest(),
            "".join(
                difflib.unified_diff(
                    original.splitlines(keepends=True),
                    changed.splitlines(keepends=True),
                    fromfile=f"a/{self.path}",
                    tofile=f"b/{self.path}",
                )
            ),
        )
        path.write_text(changed)
        return record

    def restore(self, source: Path, record: EditRecord) -> None:
        path = source / self.path
        if record.path != self.path or sha256(path) != record.after_sha256:
            raise ValueError("edited source changed; refusing to overwrite it")
        original = path.read_text().replace(self.after, self.before, 1)
        if (
            hashlib.sha256(original.encode()).hexdigest()
            != record.before_sha256
        ):
            raise ValueError(
                "restored source does not match original checksum"
            )
        path.write_text(original)


@dataclass(frozen=True)
class Target:
    id: str
    artifact: str
    kind: Literal["static", "shared", "executable"]
    symbol_prefixes: tuple[str, ...]
    symbol_contains: tuple[str, ...]
    definitions: tuple[str, ...]
    validation_argv: tuple[tuple[str, ...], ...]
    # Build-system identity, for direct-object and static-link dependency walks.
    build_target: str
    # Candidate project paths, not a claim that every matching file is linked.
    # Codemodel / verbose link commands identify actual direct inputs.
    project_sources: tuple[str, ...]
    coverage_boundary: str = (
        "project-owned compiled sources; external shared libraries, runtime, "
        "prebuilt standard libraries and assembly are separate coverage limits"
    )


@dataclass(frozen=True)
class Recipe:
    profile_id: str
    project: str
    build_system: BuildSystem
    repository_url: str
    commit: str
    required_submodules: tuple[str, ...]
    cxx: bool
    cmake_flags: tuple[str, ...]
    autotools_flags: tuple[str, ...]
    build_targets: tuple[str, ...]
    selected_target_id: str
    edit: Edit
    lockfile: Path | None = None

    @property
    def required_dependencies(self) -> tuple[str, ...]:
        if not self.cxx:
            return ()
        return ("libev", "openssl", "zlib") + (
            ("libcares",) if self.build_system == "autotools" else ()
        )

    @property
    def required_tools(self) -> tuple[str, ...]:
        common = (
            "git",
            "clang",
            "clang++",
            "llvm-ar",
            "llvm-ranlib",
            "llvm-config",
            "llvm-dis",
            "llvm-link",
            "llvm-nm",
            "opt",
            "llvm-objcopy",
            "rllvm-cc",
            "rllvm-cxx",
            "rllvm-get-bc",
            "rllvm-info",
        )
        extra = {
            "cmake": ("cmake", "ctest", "ninja"),
            "autotools": ("autoreconf", "autoconf", "automake", "make"),
            "cargo": (
                "cargo",
                "rustc",
                "rllvm-rustc",
                "cmake",
                "ninja",
                "make",
            ),
        }[self.build_system]
        return common + extra + (("pkg-config",) if self.cxx else ())

    def targets(self, host: str) -> tuple[Target, ...]:
        if host not in ("darwin", "linux"):
            raise ToolchainError("workflow benchmarks support macOS and Linux")
        extension = "dylib" if host == "darwin" else "so"
        cargo = self.build_system == "cargo"
        libdir = (
            "release"
            if cargo
            else "lib/.libs"
            if self.build_system == "autotools"
            else "lib"
        )
        project = self.project
        sources = ("quiche/src/*.rs",) if cargo else ("lib/*.c",)
        prefix = (f"{project}_",)
        contains = ("quiche",) if cargo else ()
        targets = [
            Target(
                kind,
                f"{libdir}/lib{project}.{suffix}",
                kind,
                prefix,
                contains,
                (f"{project}_version",),
                (),
                f"{project}_static" if kind == "static" else project,
                sources,
            )
            for kind, suffix in (("static", "a"), ("shared", extension))
        ]
        if cargo:
            targets.append(
                Target(
                    "client",
                    "release/examples/client",
                    "executable",
                    prefix,
                    contains,
                    ("main",),
                    (("{build}/release/examples/client",),),
                    "client",
                    ("quiche/examples/client.rs",),
                )
            )
        else:
            targets.append(
                Target(
                    "tests",
                    "tests/main",
                    "executable",
                    prefix,
                    (),
                    ("main",),
                    (("{build}/tests/main",),),
                    "main",
                    ("tests/*.c",),
                )
            )
        if self.cxx:
            for app in ("nghttp", "h2load"):
                directory = (
                    "src/.libs" if self.build_system == "autotools" else "src"
                )
                targets.append(
                    Target(
                        app,
                        f"{directory}/{app}",
                        "executable",
                        (),
                        (f"{app}", "nghttp2"),
                        ("main",),
                        ((f"{{build}}/src/{app}", "--version"),),
                        app,
                        ("src/*.cc", "third-party/*.c"),
                    )
                )
        return tuple(targets)

    def manifest(self) -> dict[str, Any]:
        value = asdict(self)
        value["lockfile"] = str(self.lockfile) if self.lockfile else None
        return value

    @classmethod
    def from_manifest(cls, value: Mapping[str, Any]) -> Recipe:
        data = dict(value)
        for key in (
            "required_submodules",
            "cmake_flags",
            "autotools_flags",
            "build_targets",
        ):
            data[key] = tuple(data[key])
        data["edit"] = Edit(**data["edit"])
        data["lockfile"] = Path(data["lockfile"]) if data["lockfile"] else None
        return cls(**data)

    def prepare_build_evidence(self, build: Path) -> dict[str, str]:
        """Untimed setup. Call after each build reset, before configure."""
        build.mkdir(parents=True, exist_ok=True)
        if self.build_system == "cmake":
            query = build / ".cmake/api/v1/query/codemodel-v2"
            query.parent.mkdir(parents=True, exist_ok=True)
            query.touch()
            return {
                "codemodel": str(build / ".cmake/api/v1/reply"),
                "compile_commands": str(build / "compile_commands.json"),
            }
        if self.build_system == "autotools":
            return {
                "makefiles": str(build),
                "link_commands": "build stdout",
                "libtool": str(build / "libtool"),
            }
        return {
            "compiler_artifacts": "build stdout JSON messages",
            "rustc_commands": "build stderr verbose commands",
        }

    def _environment(
        self,
        build: Path,
        tools: Toolchain,
        jobs: int,
        env: Mapping[str, str],
        wrapped: bool,
    ) -> dict[str, str]:
        if jobs < 1:
            raise ValueError("jobs must be positive")
        if "RLLVM_CONFIG" not in env:
            raise ValueError("an explicit scratch RLLVM_CONFIG is required")
        result = dict(env)
        cc = tools.path("rllvm-cc" if wrapped else "clang")
        cxx = tools.path("rllvm-cxx" if wrapped else "clang++")
        # Autoconf parses tools as shell commands; cc-rs first recognizes an
        # exact executable path. Quote only for the former boundary.
        quote = shlex.quote if self.build_system == "autotools" else str
        result.update(
            CC=quote(cc),
            CXX=quote(cxx),
            AR=quote(tools.path("llvm-ar")),
            RANLIB=quote(tools.path("llvm-ranlib")),
            CFLAGS="-O2 -g",
            CXXFLAGS="-O2 -g",
            CMAKE_GENERATOR=tools.generator,
            CMAKE_BUILD_PARALLEL_LEVEL=str(jobs),
        )
        required = set(self.required_dependencies)
        found = {dependency.name for dependency in tools.dependencies}
        if required - found:
            raise ToolchainError(
                f"missing dependencies: {sorted(required - found)}"
            )
        if required:
            prefixes = [
                Path(d.prefix)
                for d in tools.dependencies
                if d.name in required
            ]
            result["CPPFLAGS"] = " ".join(
                shlex.quote(f"-I{p}/include") for p in prefixes
            )
            result["LDFLAGS"] = " ".join(
                shlex.quote(f"-L{p}/lib") for p in prefixes
            )
            result["PKG_CONFIG_LIBDIR"] = os.pathsep.join(
                str(p / subdir)
                for p in prefixes
                for subdir in (
                    "lib/pkgconfig",
                    "lib64/pkgconfig",
                    "share/pkgconfig",
                )
            )
            result["PKG_CONFIG_PATH"] = ""
        if self.build_system == "cargo":
            result.update(
                CARGO_TARGET_DIR=str(build.absolute()),
                CARGO_INCREMENTAL="0",
                CARGO_BUILD_JOBS=str(jobs),
                CARGO_NET_OFFLINE="true",
                RUSTC=tools.path("rustc"),
                RUSTC_WRAPPER=tools.path("rllvm-rustc") if wrapped else "",
                RUSTC_WORKSPACE_WRAPPER="",
                RUSTFLAGS="",
                CARGO_ENCODED_RUSTFLAGS="",
                CARGO_PROFILE_RELEASE_DEBUG="2",
                CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_DEBUG="2",
            )
            if tools.rust_host:
                target_key = tools.rust_host.upper().replace("-", "_")
                result[f"CARGO_TARGET_{target_key}_LINKER"] = tools.path(
                    "clang"
                )
        return result

    def configure_commands(
        self,
        source: Path,
        build: Path,
        tools: Toolchain,
        *,
        jobs: int,
        env: Mapping[str, str],
        wrapped: bool = False,
    ) -> tuple[Command, ...]:
        environment = self._environment(build, tools, jobs, env, wrapped)
        if self.build_system == "cargo":
            return ()
        if self.build_system == "autotools":
            return (
                Command(
                    (str(source / "configure"), *self.autotools_flags),
                    build,
                    environment,
                ),
            )
        argv = (
            tools.path("cmake"),
            "-S",
            str(source),
            "-B",
            str(build),
            "-G",
            tools.generator,
            "-DCMAKE_C_COMPILER="
            + tools.path("rllvm-cc" if wrapped else "clang"),
            "-DCMAKE_CXX_COMPILER="
            + tools.path("rllvm-cxx" if wrapped else "clang++"),
            "-DCMAKE_AR=" + tools.path("llvm-ar"),
            "-DCMAKE_RANLIB=" + tools.path("llvm-ranlib"),
            "-DCMAKE_MAKE_PROGRAM=" + tools.path("ninja"),
            "-DCMAKE_BUILD_TYPE=RelWithDebInfo",
            "-DCMAKE_C_FLAGS_RELWITHDEBINFO=-O2 -g -DNDEBUG",
            "-DCMAKE_CXX_FLAGS_RELWITHDEBINFO=-O2 -g -DNDEBUG",
            "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON",
            *self.cmake_flags,
        )
        if self.cxx:
            argv += (
                "-DCMAKE_PREFIX_PATH="
                + ";".join(
                    d.prefix
                    for d in tools.dependencies
                    if d.name in self.required_dependencies
                ),
            )
        return (Command(argv, build, environment),)

    def build_commands(
        self,
        source: Path,
        build: Path,
        tools: Toolchain,
        *,
        jobs: int,
        env: Mapping[str, str],
        wrapped: bool = False,
    ) -> tuple[Command, ...]:
        environment = self._environment(build, tools, jobs, env, wrapped)
        if self.build_system == "cmake":
            return (
                Command(
                    (
                        tools.path("cmake"),
                        "--build",
                        str(build),
                        "--parallel",
                        str(jobs),
                        "--verbose",
                        "--target",
                        *self.build_targets,
                    ),
                    build,
                    environment,
                ),
            )
        if self.build_system == "autotools":
            steps = [("lib",)]
            if self.cxx:
                steps.extend([("third-party",), ("src", "nghttp", "h2load")])
            steps.append(("tests", "main"))
            return tuple(
                Command(
                    (
                        tools.path("make"),
                        "--print-directory",
                        "-C",
                        directory,
                        f"-j{jobs}",
                        "V=1",
                        *targets,
                    ),
                    build,
                    environment,
                )
                for directory, *targets in steps
            )
        return (
            Command(
                (
                    tools.path("cargo"),
                    "build",
                    "--frozen",
                    "--release",
                    "-p",
                    "quiche",
                    "--features",
                    "ffi",
                    "--lib",
                    "--example",
                    "client",
                    "--jobs",
                    str(jobs),
                    "-vv",
                    "--message-format=json-render-diagnostics",
                    "--config",
                    "profile.release.codegen-units=1",
                    "--config",
                    "profile.release.build-override.codegen-units=1",
                ),
                source,
                environment,
            ),
        )

    def behavior_commands(
        self,
        source: Path,
        build: Path,
        tools: Toolchain,
        *,
        jobs: int,
        env: Mapping[str, str],
        wrapped: bool = False,
    ) -> tuple[Command, ...]:
        environment = self._environment(build, tools, jobs, env, wrapped)
        return tuple(
            Command(
                tuple(arg.format(build=build, source=source) for arg in argv),
                build,
                environment,
            )
            for target in self.targets(tools.host)
            for argv in target.validation_argv
        )

    def version_probe_source(self) -> str:
        header = (
            "quiche.h"
            if self.project == "quiche"
            else f"{self.project}/{self.project}.h"
        )
        function = f"{self.project}_version"
        call = (
            "version()"
            if self.project == "quiche"
            else "version(0)->version_str"
        )
        # Load the exact artifact: Cargo's Darwin install name can name a
        # versioned library for which the build does not create a symlink.
        return (
            f"#include <stdio.h>\n#include <dlfcn.h>\n#include <{header}>\n"
            "int main(int argc, char **argv){\n"
            "if(argc!=2)return 2;\n"
            "void *handle=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL);\n"
            'if(!handle){fprintf(stderr,"%s\\n",dlerror());return 3;}\n'
            f"__typeof__(&{function}) version="
            f'(__typeof__(&{function}))dlsym(handle,"{function}");\n'
            'if(!version){fprintf(stderr,"%s\\n",dlerror());return 4;}\n'
            f"puts({call});dlclose(handle);return 0;}}\n"
        )

    def version_probe_commands(
        self,
        source: Path,
        build: Path,
        probe_source: Path,
        output: Path,
        tools: Toolchain,
        *,
        env: Mapping[str, str],
    ) -> tuple[Command, ...]:
        """Caller writes version_probe_source() to an owned untimed file."""
        library = next(t for t in self.targets(tools.host) if t.id == "shared")
        path = build / library.artifact
        runtime_env = dict(env)
        runtime_env[
            "DYLD_LIBRARY_PATH"
            if tools.host == "darwin"
            else "LD_LIBRARY_PATH"
        ] = str(path.parent)
        includes = (
            (source / "quiche/include",)
            if self.project == "quiche"
            else (source / "lib/includes", build / "lib/includes")
        )
        return (
            Command(
                (
                    tools.path("clang"),
                    str(probe_source),
                    *(f"-I{p}" for p in includes),
                    *(("-ldl",) if tools.host == "linux" else ()),
                    "-o",
                    str(output),
                ),
                build,
                dict(env),
            ),
            Command((str(output), str(path)), build, runtime_env),
        )


_PINS = {
    "nghttp2": "a49ccc728863e7d8e5da369552e889e95399bd37",
    "nghttp3": "f1e4328b9afd4982ee3e9fd822a605012e3d14b3",
    "ngtcp2": "72a85865dd3a4b33fe5830baf461ed50ebb7ad0e",
    "quiche": "c8da372daa06b7cb51aa23b1a55bfe395dcf3d46",
}
DEFAULT_PROFILES = ("nghttp2-c-cmake", "nghttp2-cxx-cmake", "quiche-cargo")


def recipes() -> tuple[Recipe, ...]:
    result = []
    for project in ("nghttp2", "nghttp3", "ngtcp2"):
        for system in ("cmake", "autotools"):
            for cxx in (False, True) if project == "nghttp2" else (False,):
                result.append(_ng_recipe(project, system, cxx))
    result.append(
        Recipe(
            "quiche-cargo",
            "quiche",
            "cargo",
            "https://github.com/cloudflare/quiche",
            _PINS["quiche"],
            (),
            False,
            (),
            (),
            ("quiche", "client"),
            "static",
            Edit(
                "quiche/src/ffi.rs",
                'concat!(env!("CARGO_PKG_VERSION"), "\\0")',
                'concat!(env!("CARGO_PKG_VERSION"), "-rllvm-benchmark\\0")',
            ),
            Path(__file__).parent / "fixtures/quiche/Cargo.lock",
        )
    )
    return tuple(result)


def get_recipe(profile_id: str) -> Recipe:
    for recipe in recipes():
        if recipe.profile_id == profile_id:
            return recipe
    raise ValueError(f"unknown benchmark profile: {profile_id}")


def _ng_recipe(project: str, system: BuildSystem, cxx: bool) -> Recipe:
    cmake = [
        "-DBUILD_TESTING=ON",
        "-DENABLE_LIB_ONLY=" + ("OFF" if cxx else "ON"),
    ]
    auto = ["--enable-static", "--enable-shared"]
    if not cxx:
        auto.append("--enable-lib-only")
    submodules = ("tests/munit",)
    if project == "nghttp2":
        cmake += [
            "-DBUILD_STATIC_LIBS=ON",
            "-DBUILD_SHARED_LIBS=ON",
            "-DENABLE_FAILMALLOC=OFF",
            "-DENABLE_DOC=OFF",
            "-DENABLE_APP=" + ("ON" if cxx else "OFF"),
            "-DENABLE_EXAMPLES=OFF",
            "-DENABLE_HPACK_TOOLS=OFF",
            "-DENABLE_HTTP3=OFF",
            "-DWITH_LIBXML2=OFF",
            "-DWITH_JEMALLOC=OFF",
            "-DWITH_MRUBY=OFF",
            "-DWITH_NEVERBLEED=OFF",
            "-DWITH_WOLFSSL=OFF",
        ]
        disabled = [
            "Libcares",
            "Libbrotlienc",
            "Libbrotlidec",
            "Libngtcp2",
            "Libnghttp3",
            "Libbpf",
            "Systemd",
            "Jansson",
            "Libevent",
        ]
        if not cxx:
            disabled += ["OpenSSL", "Libev", "ZLIB"]
        cmake += [
            f"-DCMAKE_DISABLE_FIND_PACKAGE_{name}=ON" for name in disabled
        ]
        auto += [
            "--disable-failmalloc",
            "--disable-examples",
            "--disable-hpack-tools",
            "--disable-http3",
            "--with-libcares" if cxx else "--without-libcares",
            "--enable-app" if cxx else "--disable-app",
        ]
        auto += [
            f"--without-{name}"
            for name in (
                "libxml2",
                "jemalloc",
                "mruby",
                "neverbleed",
                "libngtcp2",
                "libnghttp3",
                "libbpf",
                "libbrotlienc",
                "libbrotlidec",
                "jansson",
                "libevent-openssl",
                "systemd",
                "wolfssl",
            )
        ]
        auto += [
            f"--{'with' if cxx else 'without'}-{name}"
            for name in ("libev", "openssl", "zlib")
        ]
        if cxx:
            submodules += ("third-party/urlparse",)
    else:
        cmake += ["-DENABLE_STATIC_LIB=ON", "-DENABLE_SHARED_LIB=ON"]
        if project == "nghttp3":
            submodules += ("lib/sfparse",)
        else:
            cmake += [
                f"-DENABLE_{name}=OFF"
                for name in (
                    "OPENSSL",
                    "GNUTLS",
                    "BORINGSSL",
                    "PICOTLS",
                    "WOLFSSL",
                    "JEMALLOC",
                )
            ]
            auto += ["--disable-cryptotest"]
            auto += [
                f"--without-{name}"
                for name in (
                    "openssl",
                    "gnutls",
                    "boringssl",
                    "picotls",
                    "wolfssl",
                    "libev",
                    "libnghttp3",
                    "jemalloc",
                    "libbrotlienc",
                    "libbrotlidec",
                )
            ]
    marker = f"{project.upper()}_VERSION"
    before = marker + ("};" if project == "ngtcp2" else ",")
    after = marker + f' "{BENCHMARK_SUFFIX}"' + before[len(marker) :]
    language = "cxx" if cxx else "c"
    return Recipe(
        f"{project}-{language}-{system}",
        project,
        system,
        f"https://github.com/{project}/{project}",
        _PINS[project],
        submodules,
        cxx,
        tuple(cmake),
        tuple(auto),
        (project, f"{project}_static", "main")
        + (("nghttp", "h2load") if cxx else ()),
        "nghttp" if cxx else "static",
        Edit(f"lib/{project}_version.c", before, after),
    )
