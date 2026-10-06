"""Drive the rllvm-query MCP server the way a client does and check the answers.

Runs two full JSON-RPC-over-stdio sessions against `rllvm-query mcp`, one per
protocol era the server supports. Each session discovers the server, lists its
tools, loads the captured object with `inventory`, and asks `defs` where
`helper` is defined. Invoked by check.sh with the output directory as argv[1].
"""

import json
import os
import subprocess
import sys

MODERN_FIELDS = ("resultType", "ttlMs", "cacheScope")


def session(requests):
    """Feed one connection a sequence of requests; return {id: frame}.

    stdout must carry protocol frames and nothing else -- a stray log line or
    banner would corrupt the stream for any client reading it, so every line
    has to parse as JSON.
    """
    stdin = "".join(json.dumps(r) + "\n" for r in requests)
    proc = subprocess.run(
        ["rllvm-query", "mcp"], input=stdin, capture_output=True, text=True
    )
    frames = {}
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        try:
            frame = json.loads(line)
        except json.JSONDecodeError:
            sys.exit(f"stdout carried a non-protocol line: {line!r}")
        frames[frame.get("id")] = frame
    return frames


def result(frames, request_id):
    frame = frames.get(request_id)
    if frame is None:
        sys.exit(f"no response for id {request_id}")
    if "error" in frame:
        sys.exit(f"id {request_id} returned an error: {frame['error']}")
    return frame["result"]


def tool_names(res):
    return {tool["name"] for tool in res.get("tools", [])}


def locates_helper(res):
    # tools/call wraps the query envelope in a text content block.
    envelope = json.loads(res["content"][0]["text"])
    for hit in envelope.get("results", []):
        location = hit.get("location", {})
        if (
            hit.get("function", {}).get("symbol") == "helper"
            and location.get("file", "").endswith("lib.c")
            and location.get("line") == 1
        ):
            return True
    return False


def check_legacy(libo):
    """A 2025-06-18 client: discover, initialize, list, inventory, defs."""
    frames = session(
        [
            {"jsonrpc": "2.0", "id": 1, "method": "server/discover"},
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "rllvm-example", "version": "0"},
                },
            },
            {"jsonrpc": "2.0", "id": 3, "method": "tools/list"},
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "inventory",
                    "arguments": {"artifact": libo},
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "defs", "arguments": {"name": "helper"}},
            },
        ]
    )

    versions = result(frames, 1).get("supportedVersions") or []
    if not versions:
        sys.exit(
            f"server/discover offered no supportedVersions: {result(frames, 1)}"
        )

    init = result(frames, 2)
    if init.get("protocolVersion") != "2025-06-18":
        sys.exit(
            f"legacy initialize reported protocolVersion {init.get('protocolVersion')!r}"
        )
    if init.get("serverInfo", {}).get("name") != "rllvm-query":
        sys.exit(
            f"legacy initialize serverInfo was {init.get('serverInfo')!r}"
        )

    names = tool_names(result(frames, 3))
    for expected in ("load_catalog", "inventory", "defs"):
        if expected not in names:
            sys.exit(f"tools/list omitted {expected}; got {sorted(names)}")

    if result(frames, 4).get("isError") is not False:
        sys.exit(f"inventory call did not succeed: {frames.get(4)}")
    if not locates_helper(result(frames, 5)):
        sys.exit(
            f"legacy defs did not locate helper at lib.c:1: {frames.get(5)}"
        )

    for request_id in range(1, 6):
        stray = [
            k
            for k in MODERN_FIELDS
            if k in frames[request_id].get("result", {})
        ]
        if stray:
            sys.exit(
                f"legacy response id {request_id} carried modern fields {stray}"
            )

    print(
        f"  legacy (2025-06-18): {len(names)} tools, defs located helper@lib.c:1, no cache envelope"
    )
    return versions


def check_modern(libo, versions):
    """Negotiate the newest offered version and check the modern cache envelope."""
    modern_version = max(versions)
    meta = {
        "io.modelcontextprotocol/protocolVersion": modern_version,
        "io.modelcontextprotocol/clientCapabilities": {},
    }

    def params(extra=None):
        out = {"_meta": meta}
        if extra:
            out.update(extra)
        return out

    frames = session(
        [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "server/discover",
                "params": params(),
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": params(),
            },
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": params(
                    {"name": "inventory", "arguments": {"artifact": libo}}
                ),
            },
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": params(
                    {"name": "defs", "arguments": {"name": "helper"}}
                ),
            },
        ]
    )

    # Cacheable results (discover, tools/list) carry the modern cache envelope.
    # cacheScope is an enum: a client rejects tools/list outright when the
    # value is not one it knows, so an invalid scope breaks the connection.
    for request_id in (1, 2):
        res = result(frames, request_id)
        if res.get("resultType") != "complete":
            sys.exit(
                f"modern id {request_id} resultType was {res.get('resultType')!r}"
            )
        if not isinstance(res.get("ttlMs"), int):
            sys.exit(
                f"modern id {request_id} ttlMs was not an integer: {res.get('ttlMs')!r}"
            )
        scope = res.get("cacheScope")
        if scope not in ("public", "private"):
            sys.exit(
                f"modern id {request_id} cacheScope was {scope!r}, expected public|private"
            )

    names = tool_names(result(frames, 2))
    for expected in ("load_catalog", "inventory", "defs"):
        if expected not in names:
            sys.exit(
                f"modern tools/list omitted {expected}; got {sorted(names)}"
            )

    if result(frames, 3).get("isError") is not False:
        sys.exit(f"modern inventory call did not succeed: {frames.get(3)}")
    if not locates_helper(result(frames, 4)):
        sys.exit(
            f"modern defs did not locate helper at lib.c:1: {frames.get(4)}"
        )

    print(
        f"  modern ({modern_version}): cacheScope valid (public|private), defs located helper@lib.c:1"
    )


def main(out):
    libo = os.path.join(out, "lib.o")
    versions = check_legacy(libo)
    check_modern(libo, versions)


if __name__ == "__main__":
    main(sys.argv[1])
