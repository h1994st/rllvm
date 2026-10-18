# Sourced by the setup scripts of the tapes that record a Claude Code session.
# Puts this checkout's tools first on PATH and names its plugin.
ROOT=$(pwd)
PLUGIN_DIR="$ROOT/plugins/rllvm"
export PATH="$ROOT/target/release:$PATH"
# A session started from inside another Claude Code session would inherit
# its name and child-session marker; start clean.
for v in $(compgen -e | grep -E '^(CLAUDE|AI_AGENT)'); do unset "$v"; done
export DISABLE_AUTOUPDATER=1
PS1='$ '
