# Hidden setup for getting-started.tape. Source it from the repo root.
ROOT=$(pwd)
export PATH="$ROOT/target/release:$PATH"
D=/tmp/rllvm-gif
rm -rf "$D" && mkdir -p "$D"
cp "$ROOT"/examples/ffi/*.c "$ROOT"/examples/ffi/*.cc "$ROOT"/examples/ffi/*.rs "$D"/
export RLLVM_CONFIG="$D/config.toml"
rllvm-init -o "$RLLVM_CONFIG" >/dev/null 2>&1
export PATH="$(sed -n 's/^llvm_bindir = "\(.*\)"/\1/p' "$RLLVM_CONFIG"):$PATH"
cd "$D"
PS1='$ '
clear
