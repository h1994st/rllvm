# Re-record from the repo root, after `cargo build --release -p rllvm -p rllvm-query`:
#   vhs docs/assets/tapes/getting-started.tape
# Needs vhs, ttyd, ffmpeg and LLVM; the setup script runs hidden and uses /tmp/rllvm-gif.
Output docs/assets/getting-started.gif

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 19
Set Width 1200
Set Height 580
Set Padding 28
Set LineHeight 1.25
Set TypingSpeed 40ms
Set CursorBlink false
Set WindowBar Colorful
Set WindowBarSize 40
Set BorderRadius 12
Set Margin 0
Set Framerate 20
Set Theme { "name": "rllvm", "background": "#232A35", "foreground": "#E4E8EF", "cursor": "#3DE8AA", "selection": "#3B4452", "black": "#1B212A", "red": "#FF6B6B", "green": "#3DE8AA", "yellow": "#F5A524", "blue": "#2F7BFF", "magenta": "#B48CFF", "cyan": "#5FD4F4", "white": "#E4E8EF", "brightBlack": "#7D8798", "brightRed": "#FF8A8A", "brightGreen": "#6CF0C0", "brightYellow": "#FFC14D", "brightBlue": "#5B98FF", "brightMagenta": "#C8A8FF", "brightCyan": "#8BE1F7", "brightWhite": "#FFFFFF" }

Hide
Type "source docs/assets/tapes/getting-started-setup.sh" Enter
Sleep 4s
Show

Type "# Build C, C++ and Rust with the rllvm wrappers" Enter
Sleep 600ms
Type "rllvm-cc -g -c c_side.c && rllvm-cxx -g -c cxx_side.cc && llvm-ar rcs libffidemo.a *.o" Enter
Sleep 1500ms
Type "rllvm-rustc -g main.rs -o app -L . -l static=ffidemo && ./app" Enter
Sleep 2500ms

Type "# Extract one whole-program module, then write a catalog" Enter
Sleep 600ms
Type "rllvm-get-bc app -o whole.bc && du -h whole.bc" Enter
Sleep 1500ms
Type "rllvm-info app --json > catalog.json" Enter
Sleep 2s
Hide
Type "clear" Enter
Sleep 500ms
Show

Type "# Who calls the Rust function rust_add? Can main reach it?" Enter
Sleep 600ms
Type "rllvm-query --catalog catalog.json callers rust_add" Enter
Sleep 3s
Type "rllvm-query --catalog catalog.json reach main::main rust_add" Enter
Sleep 3s
Sleep 3s
