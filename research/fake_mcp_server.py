"""A tiny stand-in for a real MCP tool server, for testing `fds guard`.

It speaks MCP over stdio, offers three tools, and writes every `tools/call` it
actually receives to the file named on its command line. A call the guard
refused never gets here, so that file is the proof.

    python fake_mcp_server.py received.jsonl
"""
import json
import sys

LOG = sys.argv[1] if len(sys.argv) > 1 else "received.jsonl"
TOOLS = [
    {"name": "set_temperature", "description": "Set a chamber's temperature", "inputSchema": {"type": "object"}},
    {"name": "run_sql_drop", "description": "Drop a table", "inputSchema": {"type": "object"}},
    {"name": "list_things", "description": "A harmless tool", "inputSchema": {"type": "object"}},
]


def reply(rid, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method, rid = req.get("method"), req.get("id")
    if rid is None:
        continue
    if method == "initialize":
        reply(rid, {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}})
    elif method == "tools/list":
        reply(rid, {"tools": TOOLS})
    elif method == "tools/call":
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(line + "\n")
        p = req["params"]
        reply(rid, {"content": [{"type": "text", "text": f"done: {p['name']} {json.dumps(p.get('arguments', {}), sort_keys=True)}"}]})
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found"}}) + "\n")
        sys.stdout.flush()
