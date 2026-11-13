#!/usr/bin/env python3
"""Run an isolated, offline OUP safety and interaction probe on macOS.

Usage: python3 tools/hagency/octos_oup_scoped_smoke.py --octos /path/to/octos
The binary is copied into a temporary sandbox; no user profile or account is read.
"""
import argparse, json, os, pathlib, queue, shutil, subprocess, sys, tempfile, threading, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser()
parser.add_argument('--octos', required=True, type=pathlib.Path)
args = parser.parse_args()
if sys.platform != 'darwin':
    raise SystemExit('This probe requires macOS sandbox-exec.')
temporary = tempfile.TemporaryDirectory(prefix='ho-', dir='/private/tmp')
root = pathlib.Path(temporary.name).resolve()
home, state, instance, workspace = [root / x for x in ('home', 'state', 'instance', 'workspace')]
for path in (home, state, instance, workspace, state / 'profiles'):
    path.mkdir(mode=0o700)

(workspace / 'private-sentinel').write_text('must-not-reach-model')
started, release = threading.Event(), threading.Event()
calls = []
class Model(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        data = json.dumps({'object':'list','data':[{'id':'mock-model','object':'model'}]}).encode()
        self.send_response(200); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        calls.append(body); index=len(calls); (root / f'model-{index}.json').write_text(json.dumps(body)); started.set(); release.wait(15)
        if body.get('stream'):
            if index == 1:
                invoked = 'discover_tools'
                invocation = {'name':'read_file'}
            elif index == 2:
                invoked = 'run_tool'
                invocation = {'name':'read_file','arguments':{'path':str(workspace / 'private-sentinel')}}
            elif index in (3,4):
                invoked = 'discover_tools'
                invocation = {'name':'hagency_file_list'}
            elif index == 5:
                invoked = 'run_tool'
                invocation = {'name':'hagency_file_list','arguments':{'path':''}}
            elif index == 6:
                invoked = 'run_tool'
                invocation = {'name':'ask_user_question','arguments':{'questions':[{'header':'Plan',
                    'question':'Which plan?', 'options':[{'label':'One','description':'First'},
                    {'label':'Two','description':'Second'}],'multi_select':False}]}}
            else:
                invocation = None
            delta = ({'role':'assistant','tool_calls':[{'index':0,'id':f'call_fixture_{index}','type':'function',
                'function':{'name':invoked,'arguments':json.dumps(invocation)}}]}
                if invocation else {'role':'assistant','content':'offline ok'})
            chunks = [
                {'id':'c1','object':'chat.completion.chunk','model':'mock-model','choices':[{'index':0,'delta':delta,'finish_reason':None}]},
                {'id':'c1','object':'chat.completion.chunk','model':'mock-model','choices':[{'index':0,'delta':{},'finish_reason':'tool_calls' if invocation else 'stop'}],'usage':{'prompt_tokens':1,'completion_tokens':1,'total_tokens':2}},
            ]
            data = ''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n'
            encoded = data.encode(); ctype = 'text/event-stream'
        else:
            encoded = json.dumps({'id':'c1','object':'chat.completion','model':'mock-model','choices':[{'index':0,'message':{'role':'assistant','content':'offline ok'},'finish_reason':'stop'}],'usage':{'prompt_tokens':1,'completion_tokens':1,'total_tokens':2}}).encode(); ctype = 'application/json'
        self.send_response(200); self.send_header('Content-Type',ctype); self.send_header('Content-Length',str(len(encoded))); self.end_headers()
        try: self.wfile.write(encoded)
        except BrokenPipeError: pass

server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
port = server.server_address[1]
(state / 'profiles' / 'coding.json').write_text(json.dumps({
    'id':'coding','name':'Test','enabled':True,'created_at':'2026-01-01T00:00:00Z','updated_at':'2026-01-01T00:00:00Z',
    'config':{'llm':{'primary':{'family_id':'local','model_id':'mock-model','route':{'base_url':f'http://127.0.0.1:{port}/v1','api_type':'openai'}}}}}))
selected_path = state / 'profiles' / 'coding.json'
selected = json.loads(selected_path.read_text())
selected['data_dir'] = str(root / 'outside-profile-data')
selected['config']['hooks'] = [{'event':'before_llm_call','command':['/bin/sh','-c',f'/usr/bin/touch {root}/hook-ran']}]
selected['config']['mcp_servers'] = [{'name':'fixture-mcp','command':'/bin/sh','args':['-c',f'/usr/bin/touch {root}/mcp-ran']}]
selected_path.write_text(json.dumps(selected))
other = json.loads(selected_path.read_text()); other['id'] = 'other'; other['name'] = 'Other'; other['data_dir'] = str(root / 'other-data')
other['config']['mcp_servers'][0]['args'] = ['-c',f'/usr/bin/touch {root}/other-mcp-ran']
(state / 'profiles' / 'other.json').write_text(json.dumps(other))
config = root / 'config.json'
config.write_text(json.dumps({'version':1,'hooks':[],'mcp_servers':[],'appui':{'sessions_in_cwd':False}}))
profile = f'''(version 1)
(allow default)
(deny file-read* file-write* (subpath "/Users"))
(deny network-outbound)
(allow network-outbound (remote tcp "localhost:{port}"))
(deny mach-lookup (global-name "com.apple.securityd"))'''
binary = root / 'octos'
shutil.copy2(args.octos, binary)
env = {'HOME':str(home), 'OCTOS_HOME':str(state), 'XDG_CONFIG_HOME':str(home / '.config'),
       'PATH':'/usr/bin:/bin', 'TMPDIR':str(root), 'LANG':'en_US.UTF-8'}
alias = root / 'instance-alias'
alias.symlink_to(state, target_is_directory=True)
invalid = subprocess.run(['/usr/bin/sandbox-exec','-p',profile,str(binary),'serve','--stdio',
    '--cwd',str(workspace),'--no-network','--host-scoped-profile','coding','--data-dir',str(state),
    '--instance-data-dir',str(alias),'--config',str(config)], cwd=workspace, env=env,
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
assert invalid.returncode != 0 and not (root / 'mcp-ran').exists(), 'aliased instance was not refused'
stderr = (root / 'stderr').open('wb')
proc = subprocess.Popen(['/usr/bin/sandbox-exec','-p',profile,str(binary),'serve','--stdio','--cwd',str(workspace),
    '--no-network','--host-scoped-profile','coding','--data-dir',str(state),'--instance-data-dir',str(instance),'--config',str(config)],
    cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, bufsize=1)
messages = queue.Queue()
def read():
    for line in proc.stdout:
        try: messages.put(json.loads(line))
        except json.JSONDecodeError: messages.put({'invalid_stdout':line[:200]})
    messages.put({'eof':True})
threading.Thread(target=read, daemon=True).start()
seen = []
def send(id, method, params):
    proc.stdin.write(json.dumps({'jsonrpc':'2.0','id':id,'method':method,'params':params}) + '\n'); proc.stdin.flush()
def wait(predicate, timeout=30):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        event=messages.get(timeout=max(.01,end-time.monotonic())); seen.append(event)
        if event.get('eof'): raise RuntimeError('process ended: ' + (root / 'stderr').read_text()[-2000:])
        if predicate(event): return event
    raise TimeoutError()
def rpc(id, method, params):
    send(id,method,params); return wait(lambda m:m.get('id')==id)
session = 'coding:local:offline-smoke'
turn = str(uuid.uuid4())
try:
    hello=rpc('hello','client_hello',{'transport':'stdio','client':'hagency','supported_features':['projection.envelope.v2','approval.typed.v1','session.workspace_cwd.v1','user_question.v1','turn.steer_active_only.v1','event.turn_steer_dropped.v1','runtime.host_scoped_profile.v1']})
    assert not hello.get('error'), hello
    features=hello['result']['capabilities']['supported_features']
    assert 'turn.steer_active_only.v1' in features and 'event.turn_steer_dropped.v1' in features and 'runtime.host_scoped_profile.v1' in features
    cross=rpc('cross','session/open',{'session_id':'other:local:unauthorized','profile_id':'other','cwd':str(workspace)})
    assert cross.get('error'), cross
    permissions=rpc('permissions','permission/profile/set',{'session_id':session,'update':{'mode':'read_only','network':'deny','approval_policy':'on-request'}})
    assert not permissions.get('error') and permissions['result']['current']['mode']=='read_only', permissions
    opened=rpc('open','session/open',{'session_id':session,'profile_id':'coding','cwd':str(workspace)})
    assert not opened.get('error') and opened['result']['opened']['workspace_root']==str(workspace), opened
    generic=['ask_user_question','discover_tools','run_tool']
    empty=rpc('empty-tools','peer/tools/register',{'session_id':session,'tools':[], 'generic_tools':generic,'call_timeout_ms':60000})
    assert not empty.get('error') and empty['result']['tools']==[] and empty['result']['generic_tools']==generic and empty['result']['applies']=='next_turn', empty
    tools=rpc('tools','peer/tools/register',{'session_id':session,'tools':[{'name':'hagency.file_list','description':'List authorized files','input_schema':{'type':'object','properties':{'path':{'type':'string'}},'required':['path'],'additionalProperties':False},'risk':'read','background':False,'outward':False}], 'generic_tools':generic,'call_timeout_ms':60000})
    assert not tools.get('error') and tools['result']['tools'][0]['model_name']=='hagency_file_list', tools
    assert tools['result']['generic_tools']==generic and tools['result']['applies']=='next_turn', tools
    params={'session_id':session,'expected_turn_id':turn,'require_active':True,'input':[{'kind':'text','text':'steer offline'}]}
    idle=rpc('idle','turn/steer',params)
    assert idle.get('error') and not calls, idle
    start=rpc('turn','turn/start',{'session_id':session,'turn_id':turn,'input':[{'kind':'text','text':'offline task'}]})
    assert start.get('result',{}).get('accepted') is True, start
    assert started.wait(20), 'local model not called'
    steered=rpc('steer','turn/steer',params)
    assert steered.get('result',{}).get('steered') is True, steered
    release.set()
    tool_call=wait(lambda m:m.get('method')=='peer/tool/call', 35)
    assert any('hagency_file_list' in str(message.get('content','')) for call in calls[2:]
        for message in call.get('messages',[]) if message.get('role')=='tool'), 'host tool was not discoverable'
    assert tool_call['params']['session_id']==session and tool_call['params']['turn_id']==turn, tool_call
    assert tool_call['params']['name']=='hagency.file_list' and tool_call['params']['args']=={'path':''}, tool_call
    tool_reply=rpc('tool-result','peer/tool/result',{'session_id':session,'call_id':tool_call['params']['call_id'],'ok':True,'data':{'entries':['offline.txt']}})
    assert tool_reply.get('result',{}).get('accepted') is True, tool_reply
    question=wait(lambda m:m.get('method')=='user_question/requested', 35)
    assert question['params']['session_id']==session and question['params']['turn_id']==turn, question
    assert question['params']['questions'][0]['question']=='Which plan?', question
    answer=rpc('answer','user_question/respond',{'session_id':session,'question_id':question['params']['question_id'],
        'answers':[{'selected_labels':['One']}]})
    assert answer.get('result',{}).get('accepted') is True, answer
    terminal=wait(lambda m:m.get('method')=='projection/envelope' and m.get('params',{}).get('payload',{}).get('type')=='turn_terminal', 45)
    assert not (root / 'hook-ran').exists() and not (root / 'mcp-ran').exists() and not (root / 'other-mcp-ran').exists(), 'scoped profile ran an extension'
    assert not (root / 'outside-profile-data').exists() and not (root / 'other-data').exists(), 'scoped profile opened unrelated state'
    assert not any('must-not-reach-model' in json.dumps(call) for call in calls), 'restricted native file tool leaked content'
    assert any('unavailable' in json.dumps(call.get('messages',[])).lower() for call in calls[1:3]), 'native file tool was not refused'
    prior=len(calls)
    assert all(not any(t.get('function',{}).get('name') in ('read_file','write_file','edit_file','exec_command')
        for t in c.get('tools',[])) for c in calls), 'native file or shell tool exposed'
    ended=rpc('ended','turn/steer',params)
    assert ended.get('error'), ended
    time.sleep(.5)
    assert len(calls)==prior, 'ended steer launched another model call'
    print(json.dumps({'result':'ok','features':True,'read_only':True,'host_tool_registered':True,
        'idle_steer_rejected':True,'active_steer_accepted':True,'ended_steer_rejected':True,
        'model_calls':prior,'terminal':terminal['params']['payload']['data'].get('outcome'),
        'host_tool_discovered':True,'host_tool_invoked_and_answered':True,'question_answered':True,
        'model_tools':[[t.get('function',{}).get('name') for t in c.get('tools',[])] for c in calls], 'scope_checks':True}))
finally:
    release.set()
    try: proc.stdin.close()
    except BrokenPipeError: pass
    try: proc.wait(timeout=5)
    except subprocess.TimeoutExpired: proc.terminate(); proc.wait(timeout=5)
    server.shutdown(); stderr.close(); temporary.cleanup()
