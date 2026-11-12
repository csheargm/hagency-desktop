#!/usr/bin/env python3
"""Disposable stdio metadata server. Never reads files, calls tools or networks."""
import json
import sys

tools = json.loads(sys.argv[-1]) if sys.argv[-1].startswith("[") else None
if tools is None:
    # Rust writes this literal into a private fixture copy before launching it.
    tools = []  # HAGENCY_FIXTURE_TOOLS
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "offline-fixture", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": name, "description": "Offline metadata fixture", "inputSchema": {"type": "object"}}
                            for name in tools]}
    elif method == "ping":
        result = {}
    else:
        raise RuntimeError("Only metadata requests are allowed")
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
