# Hidden setup for callgraph-overlay-cli.tape. Source it from the repo root.
ROOT=$(pwd)
export PATH="$ROOT/target/release:$PATH"
D=/tmp/rllvm-overlay-cli
rm -rf "$D" && mkdir -p "$D"
cp "$ROOT"/examples/callgraph-overlay/*.c "$ROOT"/examples/value-guards/clamp.c "$D"/
export RLLVM_CONFIG="$D/config.toml"
rllvm-init -o "$RLLVM_CONFIG" >/dev/null 2>&1
export PATH="$(sed -n 's/^llvm_bindir = "\(.*\)"/\1/p' "$RLLVM_CONFIG"):$PATH"
# The physical path, so `file:line` sites resolve and answers print them relative.
cd -P "$D"
rllvm-cc -g -O0 -c ops.c && rllvm-cc -g -O0 -c app.c && rllvm-cc ops.o app.o -o app
rllvm-info app --json >catalog.json
rllvm-cc -g -O0 clamp.c -o clamp && rllvm-info clamp --json >clamp.json
PS1='$ '
PS2='> '
clear
