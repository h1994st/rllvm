#!/usr/bin/env bash
# Verifies the Claude Code plugin in plugins/rllvm: the MCP server it declares
# starts and answers, its scripts behave, and its skills are well formed.
set -euo pipefail
source "$(dirname "$0")/../common.sh"

require rllvm-query python3

REPO=$(cd "$(dirname "$0")/../.." && pwd)
PLUGIN=$REPO/plugins/rllvm
mkdir -p "$OUT"

# The server entry is read from the plugin's own .mcp.json, so this proves
# what ships rather than a hand-written copy of it.
python3 - "$PLUGIN/.mcp.json" "$OUT/tools.json" <<'PY'
import json, subprocess, sys

server = json.load(open(sys.argv[1]))["mcpServers"]["rllvm-query"]
request = '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n'
reply = subprocess.run(
    [server["command"], *server["args"]],
    input=request, capture_output=True, text=True, check=True,
).stdout
open(sys.argv[2], "w").write(reply)
names = {tool["name"] for tool in json.loads(reply)["result"]["tools"]}
for expected in ("load_catalog", "inventory"):
    if expected not in names:
        sys.exit(f"the plugin's server omitted {expected}; got {sorted(names)}")
PY
echo "ok: the plugin's MCP entry starts rllvm-query and lists its tools"

# doctor.sh on the harness's working configuration: no problem with the tools
# or the config. A version mismatch on this machine would be a real one, so
# only the tool and config lines are asserted.
"$PLUGIN/scripts/doctor.sh" >"$OUT/doctor-good.txt" || true
doctor=$(cat "$OUT/doctor-good.txt")
defines "$doctor" '^problems: [0-9]+$' "doctor.sh printed no problem count"
if grep -qE '^problem: (rllvm-[a-z-]+ is not on PATH|[a-z_]+_filepath|no config)' <<<"$doctor"; then
    fail "doctor.sh flagged a working setup: $doctor"
fi

# A config naming a clang that does not exist is a problem, named by key.
sed 's|^clang_filepath = .*|clang_filepath = "/nonexistent/clang"|' \
    "$RLLVM_CONFIG" >"$OUT/broken.toml"
status=0
RLLVM_CONFIG=$OUT/broken.toml "$PLUGIN/scripts/doctor.sh" >"$OUT/doctor-broken.txt" || status=$?
[ "$status" = 1 ] || fail "doctor.sh exited $status on a broken config"
defines "$(cat "$OUT/doctor-broken.txt")" '^problem: clang_filepath' \
    "doctor.sh did not name clang_filepath"

# A rustc whose LLVM is newer than the capture LLVM cannot be merged.
mkdir -p "$OUT/fakebin"
printf '#!/bin/sh\necho "LLVM version: 999.0.0"\n' >"$OUT/fakebin/rustc"
chmod +x "$OUT/fakebin/rustc"
PATH=$OUT/fakebin:$PATH "$PLUGIN/scripts/doctor.sh" >"$OUT/doctor-rust.txt" || true
defines "$(cat "$OUT/doctor-rust.txt")" '^problem: rustc .*999' \
    "doctor.sh missed a rustc LLVM newer than its readers"

# No config at all is reported, and doctor.sh does not create one.
status=0
RLLVM_CONFIG=$OUT/absent.toml "$PLUGIN/scripts/doctor.sh" >"$OUT/doctor-none.txt" || status=$?
[ "$status" = 1 ] || fail "doctor.sh exited $status with no config"
[ ! -e "$OUT/absent.toml" ] || fail "doctor.sh wrote a config"

# A config whose clang_filepath is a directory, not a file, is a problem too.
mkdir -p "$OUT/dirbin"
sed "s|^clang_filepath = .*|clang_filepath = \"$OUT/dirbin\"|" \
    "$RLLVM_CONFIG" >"$OUT/broken-dir.toml"
status=0
RLLVM_CONFIG=$OUT/broken-dir.toml "$PLUGIN/scripts/doctor.sh" >"$OUT/doctor-broken-dir.txt" || status=$?
[ "$status" = 1 ] || fail "doctor.sh exited $status on a directory clang_filepath"
defines "$(cat "$OUT/doctor-broken-dir.txt")" '^problem: clang_filepath' \
    "doctor.sh accepted a directory as clang_filepath"

# A rustc reporting a non-numeric LLVM version cannot be compared, and
# doctor.sh must say so without ever reaching a shell arithmetic comparison.
mkdir -p "$OUT/fakebin-unreadable"
printf '#!/bin/sh\necho "LLVM version: git-abcdef"\n' >"$OUT/fakebin-unreadable/rustc"
chmod +x "$OUT/fakebin-unreadable/rustc"
PATH=$OUT/fakebin-unreadable:$PATH "$PLUGIN/scripts/doctor.sh" \
    >"$OUT/doctor-rust-unreadable.txt" 2>"$OUT/doctor-rust-unreadable.stderr" || true
defines "$(cat "$OUT/doctor-rust-unreadable.txt")" \
    '^problem: cannot read an LLVM version from rustc' \
    "doctor.sh did not flag an unreadable rustc LLVM version"
[ ! -s "$OUT/doctor-rust-unreadable.stderr" ] ||
    fail "doctor.sh wrote to stderr: $(cat "$OUT/doctor-rust-unreadable.stderr")"

echo "ok: doctor.sh reports tools, config and LLVM versions"

# The pipeline end to end, in a temporary directory it must remove.
mkdir -p "$OUT/tmp"
TMPDIR=$OUT/tmp "$PLUGIN/scripts/smoke-test.sh" >"$OUT/smoke.txt" 2>&1 ||
    fail "smoke-test.sh failed: $(cat "$OUT/smoke.txt")"
smoke=$(cat "$OUT/smoke.txt")
defines "$smoke" '^ok: rllvm-get-bc extracts' "smoke-test.sh skipped extraction"
defines "$smoke" '^ok: rllvm-query inventories' "smoke-test.sh skipped the query"
[ -z "$(ls -A "$OUT/tmp")" ] || fail "smoke-test.sh left $(ls "$OUT/tmp") behind"

# With no config it refuses rather than letting a wrapper write one.
status=0
RLLVM_CONFIG=$OUT/absent.toml TMPDIR=$OUT/tmp "$PLUGIN/scripts/smoke-test.sh" \
    >"$OUT/smoke-none.txt" 2>&1 || status=$?
[ "$status" = 1 ] || fail "smoke-test.sh exited $status with no config"
[ ! -e "$OUT/absent.toml" ] || fail "smoke-test.sh let a wrapper write a config"
echo "ok: smoke-test.sh runs the pipeline and cleans up"

# Every skill: a header naming its directory with a description, and every
# script it points at is resolved through ${CLAUDE_SKILL_DIR} and exists.
python3 - "$PLUGIN" <<'PY'
import pathlib, re, sys

plugin = pathlib.Path(sys.argv[1])
skills = sorted((plugin / "skills").glob("*/SKILL.md"))
expected = {"setup", "capture", "query"}
found = {skill.parent.name for skill in skills}
if found != expected:
    sys.exit(f"skills: expected {sorted(expected)}, found {sorted(found)}")

for skill in skills:
    text = skill.read_text()
    header = re.match(r"---\n(.*?)\n---\n", text, re.S)
    if not header:
        sys.exit(f"{skill}: no frontmatter")
    fields = dict(
        line.split(":", 1) for line in header.group(1).splitlines() if ":" in line
    )
    if fields.get("name", "").strip() != skill.parent.name:
        sys.exit(f"{skill}: name is {fields.get('name')!r}, not {skill.parent.name!r}")
    if not fields.get("description", "").strip():
        sys.exit(f"{skill}: no description")
    # Only the plugin directory is installed, so a skill can point at nothing
    # outside it: no URLs, and no Markdown links.
    outside = re.findall(r"https?://\S+|\]\([^)]*\)", text)
    if outside:
        sys.exit(f"{skill}: links outside the plugin: {outside}")
    bare = re.findall(r"scripts/[\w.-]+", text)
    resolved = re.findall(r"\$\{CLAUDE_SKILL_DIR\}/(\.\./\.\./scripts/[\w.-]+)", text)
    if len(bare) != len(resolved):
        sys.exit(f"{skill}: scripts/ mentioned without the ${{CLAUDE_SKILL_DIR}} prefix")
    for script in resolved:
        if not (skill.parent / script).resolve().is_file():
            sys.exit(f"{skill}: {script} does not exist")
PY
echo "ok: every skill is named, described, self-contained, and its scripts exist"

# The skills condense the README for agents. Every name they use must exist
# where it is defined: environment variables, config keys and packages in the
# README, flags in the tool's own help, MCP names in tools/list, and envelope
# fields in the query crate. A rename there fails here until the skills follow.
python3 - "$PLUGIN" "$REPO" "$OUT/tools.json" <<'PY'
import json, pathlib, re, shutil, subprocess, sys

plugin, repo = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
readme = (repo / "README.md").read_text()
tools = json.load(open(sys.argv[3]))["result"]["tools"]
mcp = {t["name"] for t in tools}
mcp |= {k for t in tools for k in t["inputSchema"].get("properties", {})}
config_keys = set(re.findall(r"^\| `([a-z_]+)` \| (?:Yes|No) \|", readme, re.M))
fields = {
    name
    for source in [*(repo / "crates/query/src").glob("*.rs"), repo / "crates/core/src/catalog.rs"]
    for name in re.findall(r"pub ([a-z_]+):", source.read_text())
}
# rllvm-rustc passes its arguments to rustc, whose help lists them.
EXTRA_HELP = {"rllvm-rustc": ["rustc", "--help", "-v"]}
helps = {}

def help_text(command, sub):
    if (command, sub) not in helps:
        if not shutil.which(command):
            sys.exit(f"skills name {command}, which is not installed")
        runs = [[command, sub, "--help"]] if sub else []
        runs += [[command, "--help"], [command, "--rllvm-help"]]
        runs += [EXTRA_HELP[command]] if command in EXTRA_HELP else []
        helps[command, sub] = "".join(
            subprocess.run(r, capture_output=True, text=True).stdout for r in runs
        )
    return helps[command, sub]

problems = []
for skill in sorted((plugin / "skills").glob("*/SKILL.md")):
    text = skill.read_text().split("\n---\n", 1)[1]
    fenced = re.findall(r"```\w*\n(.*?)```", text, re.S)
    spans = re.findall(r"`([^`\n]+)`", re.sub(r"```.*?```", "", text, flags=re.S))
    spans += [line.split("#")[0] for block in fenced for line in block.splitlines()]
    for span in spans:
        command = re.search(r"(?<![\w-])(rllvm-[a-z-]+)(?: ([a-z]+)\b)?", span)
        for flag in re.findall(r"(?<![\w-])--[a-z][a-z0-9-]*", span):
            # Other tools' flags (`cmake --build`) are documented in the README.
            where = readme + (help_text(*command.groups()) if command else "")
            if flag not in where:
                problems.append(f"{skill.parent.name}: {flag} in `{span}`")
        for package in re.findall(r"(?:brew|cargo|apt) install ([^;]+)", span):
            for name in package.split():
                if not name.startswith("-") and name not in readme:
                    problems.append(f"{skill.parent.name}: package {name}")
    for span in spans:
        for var in re.findall(r"\b[A-Z][A-Z0-9]*_[A-Z0-9_]+\b", span):
            if not var.startswith("CLAUDE_") and var not in readme:
                problems.append(f"{skill.parent.name}: {var} is not in the README")
        # A file name (`compile_commands.json`) is not a key.
        for key in re.findall(r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b(?!\.\w)", span):
            if key not in config_keys | mcp | fields:
                problems.append(f"{skill.parent.name}: {key} is not a config key, MCP name or envelope field")
    for dev in re.findall(r"`([a-z0-9-]*-N-dev)`", text):
        if dev not in readme:
            problems.append(f"{skill.parent.name}: package {dev}")
if problems:
    sys.exit("skills drifted from their sources:\n  " + "\n  ".join(sorted(set(problems))))
PY
echo "ok: every name the skills use exists in the README, the tools or the server"

# CI does not install Claude Code, so strict validation runs where it is.
# Validating the plugin also covers its skills.
if command -v claude >/dev/null; then
    claude plugin validate --strict "$PLUGIN" >"$OUT/validate-plugin.txt" 2>&1 ||
        fail "plugin manifest: $(cat "$OUT/validate-plugin.txt")"
    claude plugin validate --strict "$REPO" >"$OUT/validate-marketplace.txt" 2>&1 ||
        fail "marketplace manifest: $(cat "$OUT/validate-marketplace.txt")"
    echo "ok: claude plugin validate --strict passes"
else
    echo "note: claude not installed; manifests not validated"
fi
