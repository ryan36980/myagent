#!/usr/bin/env python3
"""Slow MCP server for timeout integration tests.

Same protocol as mock_mcp_server.py, but tools/call sleeps for 5 seconds
to trigger timeout behavior.
"""
import json
import sys
import time

def handle(req):
    method = req.get("method")
    params = req.get("params", {})
    rid = req.get("id")

    if method == "initialize":
        return {"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "slow-mcp", "version": "0.1.0"}
        }}

    if method == "notifications/initialized":
        return None

    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "slow_echo", "description": "Echo after delay",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}
        ]}}

    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "slow_echo":
            time.sleep(5)
            text = args.get("text", "")
            return {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": text}]
            }}
        return {"jsonrpc": "2.0", "id": rid, "error": {
            "code": -32601, "message": f"Unknown tool: {name}"
        }}

    return {"jsonrpc": "2.0", "id": rid, "error": {
        "code": -32601, "message": f"Unknown method: {method}"
    }}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        continue
    resp = handle(req)
    if resp is not None:
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
