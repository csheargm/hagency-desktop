#!/usr/bin/env python3
"""Disposable localhost Anthropic SSE fixture. No credentials or outside calls."""
import http.server
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
questions = [{"question": f"Question {i}?", "header": f"Q{i}", "multiSelect": i == 0,
              "options": [{"label": "One", "description": "First"}, {"label": "Two", "description": "Second"}]}
             for i in range(4)]


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
        results = [block for message in data.get("messages", []) for block in message.get("content", [])
                   if isinstance(block, dict) and block.get("tool_use_id") == "question-native"]
        tools = [tool.get("name") for tool in data.get("tools", [])]
        if results:
            (root / "native-answer.json").write_text(json.dumps(results))
        asking = "AskUserQuestion" in tools and not results
        if asking:
            content = {"type": "tool_use", "id": "question-native", "name": "AskUserQuestion", "input": {"questions": questions}}
        else:
            content = {"type": "text", "text": "Offline native question completed."}
        message = {"id": "msg_question" if asking else "msg_done", "type": "message", "role": "assistant",
                   "model": data.get("model", "claude-sonnet-4-5"), "content": [content],
                   "stop_reason": "tool_use" if asking else "end_turn", "stop_sequence": None,
                   "usage": {"input_tokens": 17, "output_tokens": 9, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}
        self.send_response(200)
        if not data.get("stream"):
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(message).encode())
            return
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        def event(kind, value):
            self.wfile.write(f'event: {kind}\ndata: {json.dumps({"type":kind, **value})}\n\n'.encode())
            self.wfile.flush()
        start = dict(message, content=[], stop_reason=None)
        event("message_start", {"message": start})
        if asking:
            event("content_block_start", {"index":0,"content_block":dict(content,input={})})
            event("content_block_delta", {"index":0,"delta":{"type":"input_json_delta","partial_json":json.dumps(content["input"])}})
        else:
            event("content_block_start", {"index":0,"content_block":{"type":"text","text":""}})
            event("content_block_delta", {"index":0,"delta":{"type":"text_delta","text":content["text"]}})
        event("content_block_stop", {"index":0})
        event("message_delta", {"delta":{"stop_reason":message["stop_reason"],"stop_sequence":None},"usage":{"output_tokens":9}})
        event("message_stop", {})


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
