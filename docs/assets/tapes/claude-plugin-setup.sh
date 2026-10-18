# Sourced by claude-plugin.tape (hidden). Run from the repo root after
# `cargo build --release -p rllvm -p rllvm-query`.
# Builds examples/ffi with the wrappers and writes catalog.json, so the
# recorded session only asks the question.
source docs/assets/tapes/claude-env.sh
D=/tmp/rllvm-plugin-demo
rm -rf "$D" && mkdir -p "$D/build"
cp "$ROOT"/examples/ffi/*.c "$ROOT"/examples/ffi/*.cc "$ROOT"/examples/ffi/*.rs "$D"/
export RLLVM_CONFIG="$D/build/config.toml"
rllvm-init -o "$RLLVM_CONFIG" >/dev/null 2>&1
LLVM_BIN=$(llvm-config --bindir 2>/dev/null || echo /opt/homebrew/opt/llvm/bin)
cd "$D"
rllvm-cc -g -c c_side.c -o build/c_side.o
rllvm-cxx -g -c cxx_side.cc -o build/cxx_side.o
"$LLVM_BIN/llvm-ar" rcs build/libffidemo.a build/c_side.o build/cxx_side.o
rllvm-rustc -g main.rs -o build/app -L build -l static=ffidemo
rllvm-info build/app --json >catalog.json
