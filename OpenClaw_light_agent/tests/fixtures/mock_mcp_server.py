#!/usr/bin/env python3
"""Minimal MCP server for integration tests.

Communicates over stdin/stdout using newline-delimited JSON-RPC 2.0.
Supports: initialize, notifications/initialized, tools/list, tools/call.
Tools: echo (returns input text), add (sums a+b).
"""
import json
import sys

def handle(req):
    method = req.get("method")
    params = req.get("params", {})
    rid = req.get("id")

    if method == "initialize":
        return {"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "mock-mcp", "version": "0.1.0"}
        }}

    if method == "notifications/initialized":
        return None  # notification, no response

    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "echo", "description": "Echo input text",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
            {"name": "add", "description": "Add two numbers",
             "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]}}
        ]}}

    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "echo":
            text = args.get("text", "")
            return {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": text}]
            }}
        if name == "add":
            result = args.get("a", 0) + args.get("b", 0)
            return {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": str(result)}]
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
