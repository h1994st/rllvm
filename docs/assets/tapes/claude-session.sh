#!/usr/bin/env bash
# Records docs/assets/<name>.gif from docs/assets/tapes/<name>.tape, a real
# Claude Code session (each run costs a model turn and answers differently),
# then crops and trims the recording. Run from the repo root after
# `cargo build --release -p rllvm -p rllvm-query`; needs vhs and ffmpeg.
#
#   docs/assets/tapes/claude-session.sh claude-plugin
#   docs/assets/tapes/claude-session.sh callgraph-overlay
#
# A second argument names an existing recording, to redo only the crop and trim.
set -euo pipefail

NAME=${1:?usage: claude-session.sh <tape name> [recording.mp4]}
RAW=${2:-/tmp/rllvm-$NAME.mp4}
GIF=docs/assets/$NAME.gif
HOLD=5 # seconds the finished answer stays on screen

[ $# -gt 1 ] || vhs "docs/assets/tapes/$NAME.tape"

# The answer is done when the screen stops changing for good. Freezes split
# by encoder noise (a changed frame within 0.3 s) count as one.
done_at=$(ffmpeg -i "$RAW" -vf "freezedetect=n=0.0001:d=1.5,metadata=print:file=-" \
    -f null - 2>/dev/null | awk -F= '
    /freeze_start/ { if ($2 - end > 0.3) start = $2 }
    /freeze_end/   { end = $2 }
    END            { print start }')
end=$(awk -v t="$done_at" -v h="$HOLD" 'BEGIN { print t + h }')

# Claude Code's welcome header (terminal rows 1-3, y 58-142 at the tapes'
# size) names the account's plan; cut those rows out of every frame. Later
# rows scroll through the same band, so nothing else is hidden for long.
ffmpeg -loglevel error -y -t "$end" -i "$RAW" -filter_complex "
    [0:v]split[a][b];
    [a]crop=iw:58:0:0[top];
    [b]crop=iw:ih-143:0:143[rest];
    [top][rest]vstack,fps=12,split[v][p];
    [p]palettegen=stats_mode=diff:max_colors=96[pal];
    [v][pal]paletteuse=dither=none:diff_mode=rectangle" "$GIF"
ls -l "$GIF"
