#!/usr/bin/env python3
"""Synthetic loopback OpenAI stream for the opt-in real Octos integration test."""
import json
import pathlib
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

root = pathlib.Path(sys.argv[1])
calls = []


def session_id(value):
    """Find a previous synthetic host-tool session in a model transcript."""
    if isinstance(value, dict):
        found = value.get('session_id')
        if isinstance(found, str) and found.startswith('shell-'):
            return found
        for item in value.values():
            found = session_id(item)
            if found:
                return found
    elif isinstance(value, list):
        for item in reversed(value):
            found = session_id(item)
            if found:
                return found
    elif isinstance(value, str):
        try:
            decoded = json.loads(value)
        except (ValueError, TypeError):
            return None
        if decoded != value:
            return session_id(decoded)
    return None


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        data = json.dumps({'object': 'list', 'data': [{'id': 'test-model', 'object': 'model'}]}).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        # Optional synthetic-auth fixture: persist only the comparison result,
        # never the Authorization header or its value.
        expected = root / 'offline-model-expected-authorization.txt'
        if expected.exists():
            matches = self.headers.get('Authorization') == 'Bearer ' + expected.read_text()
            (root / 'offline-model-authorization-matches.txt').write_text(str(matches))
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        calls.append(body)
        (root / 'offline-model-requests.json').write_text(json.dumps(calls))
        n = len(calls)
        script = root / 'offline-model-script.json'
        if script.exists():
            steps = json.loads(script.read_text())
            item = steps[n - 1] if n <= len(steps) else None
            name, arguments = (item['name'], item['arguments']) if item else (None, None)
            if item and arguments.get('arguments', {}).get('session_id') == '$previous_session':
                selected = session_id(body.get('messages', []))
                if not selected:
                    raise ValueError('synthetic model did not receive a shell session')
                arguments['arguments']['session_id'] = selected
        elif n == 1:
            name, arguments = 'discover_tools', {'name': 'hagency_read_room_history'}
        elif n == 2:
            name, arguments = 'run_tool', {'name': 'hagency_read_room_history', 'arguments': {'cursor': None}}
        elif n == 3:
            name, arguments = 'run_tool', {'name': 'ask_user_question', 'arguments': {'questions': [
                {'header': 'Choice', 'question': 'Which option?', 'options': [
                    {'label': 'One', 'description': 'First'}, {'label': 'Two', 'description': 'Second'}],
                 'multi_select': False}]}}
        else:
            name, arguments = None, None
        delta = ({'role': 'assistant', 'tool_calls': [{'index': 0, 'id': f'fixture-{n}', 'type': 'function',
                    'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
                 if name else {'role': 'assistant', 'content': 'Offline integration complete.'})
        chunks = [
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'model': 'test-model',
             'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'model': 'test-model',
             'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'tool_calls' if name else 'stop'}],
             'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}},
        ]
        data = (''.join('data: ' + json.dumps(chunk) + '\n\n' for chunk in chunks) + 'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
print(server.server_address[1], flush=True)
server.serve_forever()
