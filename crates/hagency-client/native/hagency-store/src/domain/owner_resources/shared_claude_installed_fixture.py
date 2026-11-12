#!/usr/bin/env python3
"""Local Anthropic wire contract only; no credentials or outside connections."""
import http.server
import json
import pathlib
import sys
import threading

root = pathlib.Path(sys.argv[1])
requests = []
credentials = []
lock = threading.Lock()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        if "count_tokens" in self.path:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"input_tokens":17}')
            return
        if not self.path.startswith("/v1/messages"):
            self.send_error(404)
            return
        with lock:
            requests.append(data)
            turn = len(requests)
            (root / "api-requests.json").write_text(json.dumps(requests, indent=2))
            # Which credential header each request carried, for the tests that
            # inject one. Both values are synthetic.
            credentials.append({name: self.headers.get(name) for name in ("authorization", "x-api-key")})
            (root / "api-credentials.json").write_text(json.dumps(credentials, indent=2))
        text = f"Shared native reply {turn}"
        message = {
            "id": f"msg_shared_{turn}", "type": "message", "role": "assistant",
            "model": data["model"], "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn", "stop_sequence": None,
            "usage": {"input_tokens": 17, "output_tokens": 9,
                      "cache_read_input_tokens": 3, "cache_creation_input_tokens": 7,
                      "cache_creation": {"ephemeral_5m_input_tokens": 5, "ephemeral_1h_input_tokens": 2},
                      "output_tokens_details": {"thinking_tokens": 4}},
        }
        self.send_response(200)
        if not data.get("stream"):
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(message).encode())
            return
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(kind, value):
            self.wfile.write(f'event: {kind}\ndata: {json.dumps({"type": kind, **value})}\n\n'.encode())
            self.wfile.flush()

        event("message_start", {"message": dict(message, content=[], stop_reason=None)})
        event("content_block_start", {"index": 0, "content_block": {"type": "text", "text": ""}})
        event("content_block_delta", {"index": 0, "delta": {"type": "text_delta", "text": text}})
        event("content_block_stop", {"index": 0})
        event("message_delta", {"delta": {"stop_reason": "end_turn", "stop_sequence": None},
                                "usage": {"output_tokens": 9, "output_tokens_details": {"thinking_tokens": 4}}})
        event("message_stop", {})


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
