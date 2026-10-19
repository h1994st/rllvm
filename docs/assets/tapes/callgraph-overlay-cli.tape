# Re-record from the repo root, after `cargo build --release -p rllvm -p rllvm-query`:
#   vhs docs/assets/tapes/callgraph-overlay-cli.tape
# Needs vhs, ttyd, ffmpeg and LLVM; the setup script runs hidden and uses /tmp/rllvm-overlay-cli.
Output docs/assets/callgraph-overlay-cli.gif

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 17
Set Width 1480
Set Height 560
Set Padding 28
Set LineHeight 1.25
Set TypingSpeed 25ms
Set CursorBlink false
Set WindowBar Colorful
Set WindowBarSize 40
Set BorderRadius 12
Set Margin 0
Set Framerate 20
Set Theme { "name": "rllvm", "background": "#232A35", "foreground": "#E4E8EF", "cursor": "#3DE8AA", "selection": "#3B4452", "black": "#1B212A", "red": "#FF6B6B", "green": "#3DE8AA", "yellow": "#F5A524", "blue": "#2F7BFF", "magenta": "#B48CFF", "cyan": "#5FD4F4", "white": "#E4E8EF", "brightBlack": "#7D8798", "brightRed": "#FF8A8A", "brightGreen": "#6CF0C0", "brightYellow": "#FFC14D", "brightBlue": "#5B98FF", "brightMagenta": "#C8A8FF", "brightCyan": "#8BE1F7", "brightWhite": "#FFFFFF" }

Hide
Type "source docs/assets/tapes/callgraph-overlay-cli-setup.sh" Enter
Wait+Line@1m /\$ *$/
Show

Type "# dispatch calls o->on_event: which functions could that field hold?" Enter
Sleep 300ms
Type "rllvm-query --catalog catalog.json resolution-candidates" Enter
Sleep 2500ms
Type "# LLVM cannot bound the call, so main does not reach handler" Enter
Sleep 300ms
Type "rllvm-query --catalog catalog.json reach main handler" Enter
Sleep 2500ms
Hide
Type "clear" Enter
Sleep 500ms
Show

Type "# An agent reads ops.c:8 and records the edge in an overlay" Enter
Sleep 300ms
Type@10ms `echo '{"op":"add","via_field":{"record":"ops","offset":8},"to":"handler","confidence":"high","provenance":["ops.c:8"]}' |` Enter
Type "rllvm-query --catalog catalog.json overlay record" Enter
Sleep 1200ms
Type "# Asked for, the overlay completes the path, labeled agent" Enter
Sleep 300ms
Type "rllvm-query --catalog catalog.json reach main handler --include-overlay" Enter
Sleep 3s
Hide
Type "clear" Enter
Sleep 500ms
Show

Type "# A verdict on a value: in clamp.c, copy() clamps the length n it passes to put" Enter
Sleep 300ms
Type@10ms `anchor='{"kind":"local","function":"copy","name":"n"}'` Enter
Type@10ms `echo '{"op":"annotate","anchor":'"$anchor"',"verdict":"infeasible","confidence":"high","provenance":["clamp.c:13"]}' |` Enter
Type "rllvm-query --catalog clamp.json overlay record" Enter
Sleep 1200ms
Type "rllvm-query --catalog clamp.json reach main put --annotations" Enter
Sleep 3s
Sleep 3s
