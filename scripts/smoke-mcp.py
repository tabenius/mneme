#!/usr/bin/env python3
"""Exercise actual stdio MCP negotiation and all eight tools, printing metadata only."""

import argparse
import json
import selectors
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--binary", default="target/debug/mneme")
parser.add_argument("--db", required=True)
parser.add_argument(
    "--semantic",
    action="store_true",
    help="Also exercise concept, LSA and hybrid retrieval; requires a built model",
)
args = parser.parse_args()

with subprocess.Popen(
    [args.binary, "--db", args.db, "mcp"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
) as process:
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)

    def send(value):
        process.stdin.write(json.dumps(value) + "\n")
        process.stdin.flush()

    def response():
        if not selector.select(15):
            raise TimeoutError("MCP response timeout")
        line = process.stdout.readline()
        assert line, "MCP server closed stdout"
        value = json.loads(line)
        assert "error" not in value, "MCP protocol error"
        return value["result"]

    send(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "mneme-smoke", "version": "0.1.0"},
            },
        }
    )
    response()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    tools = response()["tools"]
    assert len(tools) == 11
    assert all(t["annotations"]["readOnlyHint"] for t in tools)

    def call(name, arguments):
        send(
            {
                "jsonrpc": "2.0",
                "id": name,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        value = response()
        assert not value.get("isError"), f"{name} failed"
        return value["structuredContent"]

    listing = call("history_list_sessions", {"limit": 1})
    assert listing["items"], "Index has no sessions"
    sid = listing["items"][0]["session_id"]
    first = call("history_get_messages", {"session_id": sid, "limit": 1, "sort": "created_asc"})
    if first["items"]:
        call("history_get_context", {"session_id": sid, "message_id": first["items"][0]["message_id"], "window": 1})
    for name, arguments in [
        ("history_get_session", {"session_id": sid}),
        ("history_get_messages", {"session_id": sid, "limit": 1}),
        ("history_search", {"query": "session", "mode": "fts", "limit": 1}),
        ("history_get_activity", {"limit": 1}),
        ("history_get_tool_runs", {"limit": 1}),
        ("history_aggregate", {"group_by": "role"}),
        ("history_index_status", {}),
        ("history_explain_query", {"query": "repos last week"}),
        ("history_get_knowledge", {"entity": "project:mneme"}),
    ]:
        call(name, arguments)
    if args.semantic:
        for mode in ("concept", "semantic", "hybrid"):
            call("history_search", {"query": "repository", "mode": mode, "limit": 1})
    print(
        json.dumps(
            {
                "mcp_handshake": "passed",
                "tools": len(tools),
                "calls": (13 if args.semantic else 10) + (2 if first["items"] else 1),
                "read_only_annotations": True,
            }
        )
    )
    process.stdin.close()
    process.wait(timeout=10)
