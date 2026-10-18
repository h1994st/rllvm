# Sourced by callgraph-overlay.tape (hidden). Run from the repo root after
# `cargo build --release -p rllvm -p rllvm-query`.
# Builds examples/callgraph-overlay with the wrappers and writes catalog.json,
# so the recorded session only asks the question.
source docs/assets/tapes/claude-env.sh
D=/tmp/rllvm-overlay-demo
rm -rf "$D" && mkdir -p "$D/build"
cp "$ROOT"/examples/callgraph-overlay/*.c "$D"/
export RLLVM_CONFIG="$D/build/config.toml"
rllvm-init -o "$RLLVM_CONFIG" >/dev/null 2>&1
# The physical path: on macOS /tmp is a symlink, and a symlinked working
# directory keeps `file:line` sites from resolving.
cd -P "$D"
rllvm-cc -g -O0 -c ops.c -o build/ops.o
rllvm-cc -g -O0 -c app.c -o build/app.o
rllvm-cc build/ops.o build/app.o -o build/app
rllvm-info build/app --json >catalog.json
