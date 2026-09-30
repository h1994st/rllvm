#!/bin/sh
# PostToolUse hook for the rllvm-query MCP tools. Once per session, tells the
# user when rllvm-query's facts cache has grown past query_cache_warn_mb, and
# lets Claude offer to prune it. Never prunes. Silent whenever it cannot tell:
# no jq, a result that is not an rllvm-query envelope, or already warned.

command -v jq >/dev/null 2>&1 || exit 0
input=$(cat)

# The tool result arrives as a string, a CallToolResult-like object, or an
# array of content blocks; its text is the answer's JSON envelope.
cache=$(printf '%s' "$input" | jq -c '
    (.tool_response // .tool_output)
    | if type == "string" then .
      elif type == "object" then [.content[]?.text // empty] | join("")
      elif type == "array" then [.[]?.text // empty] | join("")
      else empty end
    | (fromjson? // empty)
    | (.analysis.cache // empty)
    | select(.over_threshold == true)' 2>/dev/null) || exit 0
[ -n "$cache" ] || exit 0

session=$(printf '%s' "$input" | jq -r '.session_id // "unknown"' | tr -cd 'A-Za-z0-9_-')
marker="${TMPDIR:-/tmp}/rllvm-cache-warned-${session:-unknown}"
[ -e "$marker" ] && exit 0
: >"$marker" 2>/dev/null || true

printf '%s' "$cache" | jq -c '
    ((.disk_bytes / 1048576 * 10 | round) / 10 | tostring
        | if test("\\.") then . else . + ".0" end) as $size
    | (.warn_bytes / 1048576 | floor | tostring) as $limit
    | "rllvm-query'"'"'s facts cache is \($size) MB, over query_cache_warn_mb (\($limit) MB)." as $what
    | {
        systemMessage: "\($what) Prune with `rllvm-query cache clear --stale` or `rllvm-query cache clear`.",
        hookSpecificOutput: {
          hookEventName: "PostToolUse",
          additionalContext: "\($what) Offer the user `rllvm-query cache clear --stale` (removes entries this rllvm-query no longer reads) or `rllvm-query cache clear`; do not run either without their approval."
        }
      }'
