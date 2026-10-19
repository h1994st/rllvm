# A real Claude Code session answering a question through the rllvm plugin.
# Re-record from the repo root: cargo build --release -p rllvm -p rllvm-query,
# then docs/assets/tapes/claude-session.sh claude-plugin (runs this tape, then
# crops the welcome header, which names the account's plan, and writes the GIF).
Output "/tmp/rllvm-claude-plugin.mp4"

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 18
Set Width 1200
Set Height 790
Set Padding 20
Set LineHeight 1.25
Set TypingSpeed 35ms
Set CursorBlink false
Set WindowBar Colorful
Set WindowBarSize 36
Set BorderRadius 12
Set Margin 0
Set Framerate 15
Set Theme { "name": "rllvm", "background": "#232A35", "foreground": "#E4E8EF", "cursor": "#3DE8AA", "selection": "#3B4452", "black": "#1B212A", "red": "#FF6B6B", "green": "#3DE8AA", "yellow": "#F5A524", "blue": "#2F7BFF", "magenta": "#B48CFF", "cyan": "#5FD4F4", "white": "#E4E8EF", "brightBlack": "#7D8798", "brightRed": "#FF8A8A", "brightGreen": "#6CF0C0", "brightYellow": "#FFC14D", "brightBlue": "#5B98FF", "brightMagenta": "#C8A8FF", "brightCyan": "#8BE1F7", "brightWhite": "#FFFFFF" }

Hide
Type "source docs/assets/tapes/claude-plugin-setup.sh" Enter
Wait+Line@3m /\$ *$/
Type "clear && claude --plugin-dir $PLUGIN_DIR --model claude-sonnet-5-5 --setting-sources project,local --allowedTools mcp__plugin_rllvm_rllvm-query" Enter
Sleep 6s
# Accepts the folder-trust dialog on a first run; a no-op once trusted.
Down
Sleep 300ms
Enter
Sleep 3s
Show
Sleep 1s
Type "Using catalog.json, is there a call path from main::main to rust_add? Answer briefly: each hop, its language and line."
Sleep 600ms
Enter
# The turn takes about 20 s; claude-session.sh trims the still tail.
Sleep 50s
