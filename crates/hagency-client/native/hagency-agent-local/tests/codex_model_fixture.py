"""Local Responses fixture; no credentials, remote network, or actual model."""
import http.server
import json
import threading
import time
import sys
mode = sys.argv[1] if len(sys.argv) > 1 else "children"

root_thread = None
counts = {}
sequence = 0
lock = threading.Lock()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        global root_thread, sequence
        length = int(self.headers.get('Content-Length', '0'))
        assert 0 < length <= 4 * 1024 * 1024
        body = json.loads(self.rfile.read(length))
        metadata = json.loads(self.headers['x-codex-turn-metadata'])
        thread, turn = metadata['thread_id'], metadata['turn_id']
        with lock:
            if root_thread is None:
                root_thread = thread
            is_root = thread == root_thread
            n = counts.get((thread, turn), 0)
            counts[(thread, turn)] = n + 1
            sequence += 1
            response_id = 'offline-response-' + str(sequence)
        if mode == "questions" and n == 0:
            assert any(t.get('name') == 'request_user_input' for t in body['tools'])
            item = {'type':'function_call','id':response_id+'-ask','call_id':response_id+'-ask',
                    'name':'request_user_input','arguments':json.dumps({'questions':[{'id':'db','header':'Database','question':'Which database?',
                    'options':[{'label':'SQLite','description':'Local'},{'label':'Postgres','description':'Service'}]}]})}
        elif mode == "questions":
            assert 'SQLite' in json.dumps(body['input'])
            item = {'type':'message','id':response_id+'-message','role':'assistant','status':'completed',
                    'content':[{'type':'output_text','text':'QUESTION_REPLY','annotations':[]}]}
        elif is_root and n == 0:
            namespace = next(tool['name'] for tool in body['tools']
                             if tool.get('type') == 'namespace'
                             and any(member.get('name') == 'spawn_agent' for member in tool['tools']))
            item = {'type': 'function_call', 'id': response_id + '-spawn',
                    'call_id': response_id + '-spawn', 'namespace': namespace,
                    'name': 'spawn_agent',
                    'arguments': json.dumps({'message': 'Offline child fixture', 'fork_context': True})}
        else:
            if not is_root:
                # Root completion deliberately precedes its child's receipt.
                time.sleep(0.3)
            item = {'type': 'message', 'id': response_id + '-message',
                    'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text',
                                 'text': 'ROOT_REPLY' if is_root else 'CHILD_REPLY', 'annotations': []}]}
        usage = {'input_tokens': 10, 'output_tokens': 4, 'total_tokens': 14,
                 'input_tokens_details': {'cached_tokens': 2},
                 'output_tokens_details': {'reasoning_tokens': 1}}
        response = {'id': response_id, 'object': 'response', 'status': 'completed',
                    'output': [item], 'usage': usage}
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()
        for kind, data in [
            ('response.created', {'response': {'id': response_id, 'object': 'response',
                                              'status': 'in_progress', 'output': []}}),
            ('response.output_item.done', {'output_index': 0, 'item': item}),
            ('response.completed', {'response': response}),
        ]:
            self.wfile.write(('event: ' + kind + '\ndata: ' +
                              json.dumps({'type': kind, **data}) + '\n\n').encode())
            self.wfile.flush()


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
print(server.server_port, flush=True)
server.serve_forever()
