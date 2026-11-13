#!/usr/bin/env python3
# Offline stdio actor for the Desktop bridge; no provider or network calls.
import json, sys, pathlib, os, time
mode = __MODE__
kind = __KIND__
root = pathlib.Path(__ROOT__)
continuous = mode.startswith('continuous:')
if continuous:
    mode = mode.split(':', 1)[1]
approval_tool = 'Write'
if mode.startswith('approval:'):
    _, approval_tool = mode.split(':', 1)
    mode = 'approval'

native_result_index = 0
native_last_totals = {}
def send(value):
    global native_result_index, native_last_totals
    # Each native frame is a fresh snapshot; later fixture mutations must not
    # reuse the delta attached to an already emitted result.
    value = json.loads(json.dumps(value))
    # Captured CLI 2.1.321 result metadata: breakdowns do not add billed tokens.
    if value.get('type') == 'result':
        value['uuid'] = f'fixture-result-{os.getpid()}-{native_result_index}'
        native_result_index += 1
        if 'modelUsage' not in value and value.get('usage') and mode != 'mixed_usage':
            usage = value['usage']
            value['modelUsage'] = {'main': {k:usage[v] for k,v in [('inputTokens','input_tokens'),('outputTokens','output_tokens'),('cacheReadInputTokens','cache_read_input_tokens'),('cacheCreationInputTokens','cache_creation_input_tokens')]}}
        if 'modelUsage' in value and 'model_usage_since_last_result' not in value:
            delta = {}
            for name, usage in value['modelUsage'].items():
                delta[name] = {key:usage.get(key,0)-native_last_totals.get(name,{}).get(key,0) for key in ['inputTokens','outputTokens','cacheReadInputTokens','cacheCreationInputTokens']}
            value['model_usage_since_last_result'] = delta
            native_last_totals = json.loads(json.dumps(value['modelUsage']))
        for name, usage in value.get('modelUsage', {}).items():
            usage.update({'webSearchRequests':0,'costUSD':0.001,'contextWindow':200000,
                'maxOutputTokens':64000,'thinkingTokens':min(2, usage.get('outputTokens',0)),
                'cacheCreationEphemeral5mInputTokens':usage.get('cacheCreationInputTokens',0),
                'cacheCreationEphemeral1hInputTokens':0,'canonicalModel':name,'provider':'fixture','costBasis':'synthetic'})
        for name, usage in value.get('model_usage_since_last_result', {}).items():
            usage.update({key:item for key,item in value.get('modelUsage', {}).get(name,{}).items() if key not in ['inputTokens','outputTokens','cacheReadInputTokens','cacheCreationInputTokens']})
    print(json.dumps(value), flush=True)

def read():
    value = json.loads(sys.stdin.readline())
    with (root / 'frames').open('a') as out:
        out.write(json.dumps(value) + '\n')
    return value

if sys.argv[1:3] == ['auth', 'status']:
    (root / 'auth-config').write_text(os.environ.get('CLAUDE_CONFIG_DIR', '<native-default>'))
    send({'loggedIn': True})
    sys.exit(0)
with (root / 'argv').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\n')

if kind == 'claude':
    (root / 'launch-config').write_text(os.environ.get('CLAUDE_CONFIG_DIR', '<native-default>'))
    init = read()
    if '--resume' not in sys.argv and (root / 'fresh-start-fails').exists():
        sys.exit(1)
    if '--resume' in sys.argv and (root / 'missing-context').exists():
        saved = sys.argv[sys.argv.index('--resume') + 1]
        failure = (root / 'missing-context').read_text()
        if failure == 'eof': sys.exit(1)
        if failure == 'unexpected':
            send({'type':'system','subtype':'status','session_id':saved,'status':'compacting'})
            sys.exit(1)
        if failure == 'refused':
            send({'type':'control_response','response':{'subtype':'error','request_id':init['request_id'],'error':'Resume failed'}})
            sys.exit(1)
        send({'type':'result','session_id':saved,'subtype':'error_during_execution',
            'is_error':True,'num_turns':0,'duration_api_ms':0,'total_cost_usd':0,
            'modelUsage':{},'permission_denials':[],'errors':[('Native session file cannot be opened: ' if failure == 'reworded' else 'No conversation found with session ID: ') + saved],
            'usage':{'input_tokens':0,'output_tokens':0,'cache_creation_input_tokens':0,'cache_read_input_tokens':0}})
        sys.exit(1)
    acknowledged = {'type': 'control_response', 'response': {'subtype': 'success', 'request_id': init['request_id'], 'response': {'claude_code_version':(root / 'cli-version').read_text() if (root / 'cli-version').exists() else '2.1.322'}}}
    if mode == 'future_start':
        # A newer CLI: inert metadata before the acknowledgement, and keys on
        # the acknowledgement that this host does not read.
        send({'type':'auth_status','session_id':'offline-session','isAuthenticating':False,'output':[]})
        send({'type':'rate_limit_event','session_id':'offline-session','rate_limit_info':{}})
        send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':'offline-session'})
        acknowledged['uuid'] = 'future-envelope-key'
        acknowledged['response'].update({'future_note':{'x':1},'pending_permission_requests':[],'pending_user_dialog_requests':[]})
    send(acknowledged)
    room_bound = False
    room_tools = []
    def room_metadata(ident, message):
        send({'type':'control_request','request_id':ident,'request':{'subtype':'mcp_message','server_name':'hagency_matrix','message':message}})
        return read()['response']['response']['mcp_response']
    def stop_control(refused):
        request = read()
        assert request['type'] == 'control_request' and request['request']['subtype'] == 'interrupt'
        response = {'subtype':'error','request_id':request['request_id'],'error':'refused'} if refused else {'subtype':'success','request_id':request['request_id'],'response':{'still_queued':[]}}
        send({'type':'control_response','response':response})
        return not refused
    def lifecycle(ident, state):
        send({'type':'command_lifecycle','command_uuid':ident,'state':state,'session_id':'offline-session'})
    def read_steer(prompt, acknowledge=True):
        # Native 2.1.322 acknowledges a user frame written into a running turn
        # with `queued` for that frame's own uuid.
        frame = read()
        assert frame['type'] == 'user' and frame['session_id'] == 'offline-session' and 'priority' not in frame, frame
        assert frame['uuid'] != prompt['uuid'] and frame['parent_tool_use_id'] is None, frame
        (root / 'steer-read').write_text(frame['message']['content'])
        if acknowledge:
            lifecycle(frame['uuid'], 'queued')
        return frame['uuid']
    def long_stream(session, marker):
        send({'type':'stream_event','session_id':session,'event':{'type':'message_start','message':{'id':'stream-message'}}})
        for n in range(20_000):
            send({'type':'stream_event','session_id':session,'event':{'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'streamed text '}}})
        (root / marker).write_text('flushed')
    process_count = 0
    # The owner's `/clear` opens another session; a later launch resumes it.
    resumed = sys.argv[sys.argv.index('--resume') + 1] if '--resume' in sys.argv else ''
    native_session = [resumed if resumed.startswith('cleared-session') else 'offline-session']
    while True:
        prompt = read()
        if prompt.get('type') == 'control_request':
            subtype = prompt['request']['subtype']
            if subtype == 'mcp_set_servers' and mode.startswith('room'):
                assert prompt['request']['servers'] == {'hagency_matrix':{'type':'sdk','name':'hagency_matrix','alwaysLoad':True}}
                room_metadata('room-init', {'jsonrpc':'2.0','id':0,'method':'initialize','params':{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'fixture','version':'1'}}})
                room_metadata('room-ready', {'jsonrpc':'2.0','method':'notifications/initialized'})
                room_tools = room_metadata('room-list', {'jsonrpc':'2.0','id':1,'method':'tools/list','params':{}})['result']['tools']
                room_bound = True
                response = {'added':['hagency_matrix'],'removed':[],'errors':{}}
            else:
                assert subtype == 'mcp_status' and (continuous or mode.startswith('room'))
                response = {'mcpServers':[{'name':'hagency_matrix','status':'connected','transport':'sdk','scope':'dynamic','source':'sdk','tools':[{'name':t['name']} for t in room_tools]}] if room_bound else []}
            send({'type':'control_response','response':{'subtype':'success','request_id':prompt['request_id'],'response':response}})
            continue
        if mode == 'local_command' and prompt['message']['content'] == '/clear':
            # Captured CLI 2.1.323 (offline, 2026-11-13): the reset and the
            # emptied task list come under the old session; init, an empty
            # result, the command's end and idle come under a new one.
            old, new = native_session[0], 'cleared-session'
            assert prompt['session_id'] == old, prompt
            native_session[0] = new
            zero = {'input_tokens':0,'output_tokens':0,'cache_creation_input_tokens':0,'cache_read_input_tokens':0}
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':old})
            for state in ('queued', 'started'):
                send({'type':'command_lifecycle','command_uuid':prompt['uuid'],'state':state,'session_id':old})
            send({'type':'conversation_reset','new_conversation_id':'conversation-two','uuid':'conversation-two',
                'trigger':'clear','user_message_uuid':prompt['uuid'],'timestamp':'2026-11-13T02:37:07.395Z','session_id':old})
            send({'type':'system','subtype':'background_tasks_changed','tasks':[],'session_id':old})
            # An account with a session-start hook: the CLI runs it for the
            # opened conversation and reports it under the new id, before init.
            for subtype in ('hook_started', 'hook_response'):
                send({'type':'system','subtype':subtype,'hook_id':'hook-one','hook_name':'SessionStart:clear',
                    'hook_event':'SessionStart','session_id':new})
            send({'type':'system','subtype':'init','session_id':new,'user_message_uuids':[prompt['uuid']]})
            send({'type':'result','subtype':'success','is_error':False,'num_turns':0,'duration_api_ms':0,'stop_reason':None,
                'session_id':new,'total_cost_usd':0,'usage':zero,'modelUsage':{},'model_usage_since_last_result':{},
                'permission_denials':[],'result':'','user_message_uuid':prompt['uuid'],'user_message_uuids':[prompt['uuid']],
                'local_command':'clear','queued_turn_count':0,'result_index':0})
            send({'type':'command_lifecycle','command_uuid':prompt['uuid'],'state':'completed','session_id':new})
            send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':new})
            if not continuous:
                break
            continue
        if mode == 'local_command' and prompt['message']['content'].startswith('/'):
            # Captured CLI 2.1.322, input `/cost`: init follows the command,
            # the reply is synthetic and the result carries no model totals.
            session = 'offline-session'
            text = 'Total cost:            $0.0000'
            zero = {'input_tokens':0,'output_tokens':0,'cache_creation_input_tokens':0,'cache_read_input_tokens':0}
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':session})
            lifecycle(prompt['uuid'], 'queued')
            lifecycle(prompt['uuid'], 'started')
            send({'type':'system','subtype':'init','session_id':session,'user_message_uuids':[prompt['uuid']]})
            send({'type':'assistant','session_id':session,'parent_tool_use_id':None,
                'message':{'id':'local-command-message','model':'<synthetic>','role':'assistant','type':'message',
                    'stop_reason':'end_turn','usage':zero,'content':[{'type':'text','text':text}]},
                'local_command_source':'<local-command-stdout>' + text + '</local-command-stdout>',
                'local_command_run':{'command':'usage','args':''}})
            send({'type':'result','subtype':'success','is_error':False,'num_turns':0,'duration_api_ms':0,'stop_reason':None,
                'session_id':session,'total_cost_usd':0,'usage':zero,'modelUsage':{},'model_usage_since_last_result':{},
                'permission_denials':[],'result':text,'user_message_uuid':prompt['uuid'],'user_message_uuids':[prompt['uuid']],
                'local_command':'cost','queued_turn_count':0,'result_index':0})
            lifecycle(prompt['uuid'], 'completed')
            send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':session})
            if not continuous:
                break
            continue
        process_count += 1
        session = native_session[0]
        assert prompt.get('session_id') in ('', session), prompt
        if os.environ.get('CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS') == '1':
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':session})
            send({'type':'command_lifecycle','command_uuid':'fixture-command','state':'started','session_id':session})
        send({'type': 'system', 'subtype': 'init', 'session_id': session, 'user_message_uuids':[prompt.get('uuid')]})
        aborted = False
        if mode == 'spontaneous_messages':
            send({'type':'assistant','session_id':session,'parent_tool_use_id':None,
                'message':{'id':'progress-a','content':[{'type':'text','text':'First finding'},
                    {'type':'tool_use','id':'progress-tool-a','name':'Read','input':{}}]}})
            deadline = time.monotonic() + 5
            while not (root / 'progress-observed').exists() and time.monotonic() < deadline:
                time.sleep(.01)
            assert (root / 'progress-observed').exists(), 'first message must arrive while the tool is running'
            send({'type':'user','session_id':session,'message':{'content':[{'type':'tool_result','tool_use_id':'progress-tool-a','content':'done'}]}})
            send({'type':'assistant','session_id':session,'parent_tool_use_id':'child-private',
                'message':{'id':'child-message','content':[{'type':'text','text':'private child text'}]}})
            send({'type':'assistant','session_id':session,'parent_tool_use_id':None,
                'message':{'id':'progress-b','content':[{'type':'text','text':'Second finding'},
                    {'type':'tool_use','id':'progress-tool-b','name':'Read','input':{}}]}})
            send({'type':'user','session_id':session,'message':{'content':[{'type':'tool_result','tool_use_id':'progress-tool-b','content':'done'}]}})
            send({'type':'assistant','session_id':session,'parent_tool_use_id':None,
                'message':{'id':'final','content':[{'type':'text','text':'offline reply'}]}})
        if mode in ('long_stream', 'long_stream_unknown'):
            long_stream(session, 'stream-drained')
        if mode == 'long_usage':
            for n in range(1200):
                send({'type':'assistant','session_id':session,'message':{'id':f'usage-{n}','content':[],
                    'usage':{'input_tokens':10,'output_tokens':1,'cache_read_input_tokens':0,'cache_creation_input_tokens':0}}})
        if mode.startswith('control_'):
            question = 'question' in mode
            tool = 'AskUserQuestion' if question else 'Write'
            arguments = {'questions':[{'question':'Continue?','header':'Continue','multiSelect':False,'options':[{'label':'Yes','description':'Continue'},{'label':'No','description':'Stop'}]}]} if question else {'file_path':str(root / 'outside' / 'result'),'content':'exact'}
            send({'type':'control_request','request_id':'waiting','request':{'subtype':'can_use_tool','tool_name':tool,'tool_use_id':'waiting-tool','input':arguments}})
            aborted = stop_control('refused' in mode)
            if not aborted:
                response = read()
                assert response['response']['request_id'] == 'waiting'
                assert response['response']['response']['behavior'] == 'allow'
                (root / 'effect').write_text('one')
        steers = []
        if mode == 'steer_injected':
            # Steers written while a tool waits for its owner are taken into
            # the running cycle at its next model request.
            send({'type':'control_request','request_id':'waiting','request':{'subtype':'can_use_tool','tool_name':'Write','tool_use_id':'waiting-tool','input':{'file_path':str(root / 'outside' / 'result'),'content':'exact'}}})
            for _ in range(int((root / 'steer-count').read_text())):
                steers.append(read_steer(prompt))
            response = read()
            assert response['response']['request_id'] == 'waiting' and response['response']['response']['behavior'] == 'allow'
            (root / 'effect').write_text('one')
            for ident in steers: lifecycle(ident, 'started')
            for ident in steers: lifecycle(ident, 'completed')
        if mode == 'steer_cycle':
            # A text-only turn ends its cycle first and then runs the steer as
            # a second cycle of the same turn, with no idle in between.
            steers.append(read_steer(prompt))
            send({'type':'result','subtype':'success','is_error':False,'result':'offline reply','session_id':session,
                'user_message_uuid':prompt['uuid'],'user_message_uuids':[prompt['uuid']],
                'modelUsage':{'main':{'inputTokens':10,'outputTokens':7,'cacheReadInputTokens':3,'cacheCreationInputTokens':4}}})
            lifecycle('fixture-command', 'completed')
            lifecycle(steers[0], 'started')
            send({'type':'system','subtype':'init','session_id':session,'user_message_uuids':steers})
        if mode in ('steer_stop', 'steer_stop_unacked'):
            # Stop arrives as one write: the cancel of the queued steer, then
            # the interrupt. The steer is withdrawn and never starts.
            steers.append(read_steer(prompt, mode == 'steer_stop'))
            cancel = read()
            assert cancel['type'] == 'control_request' and cancel['request'] == {'subtype':'cancel_async_message','message_uuid':steers[0]}, cancel
            if mode == 'steer_stop_unacked':
                lifecycle(steers[0], 'queued')
            lifecycle(steers[0], 'cancelled')
            send({'type':'control_response','response':{'subtype':'success','request_id':cancel['request_id'],'response':{'cancelled':True}}})
            aborted = stop_control(False)
        if mode == 'steer_exit':
            read_steer(prompt, False)
            sys.exit(0)
        if mode.startswith('room') and mode != 'room_queued_cancel':
            # An unrelated external MCP tool is never approved by Matrix IO.
            send({'type':'control_request','request_id':'external','request':{'subtype':'can_use_tool','tool_name':'mcp__external__send_file','input':{'path':'result.txt'}}})
            assert read()['response']['response']['behavior'] == 'deny'
            for index, (name, arguments) in enumerate([('read_room_history', {'cursor':None}), ('send_file', {'call_id':'generated','path':'result.txt'})]):
                tool = 'mcp__hagency_matrix__' + name
                send({'type':'control_request','request_id':f'room-permission-{index}','request':{'subtype':'can_use_tool','tool_name':tool,'tool_use_id':f'room-tool-{index}','input':arguments}})
                assert read()['response']['response']['behavior'] == 'allow'
                ident = f'room-call-{index}'
                send({'type':'control_request','request_id':ident,'request':{'subtype':'mcp_message','server_name':'hagency_matrix','message':{'jsonrpc':'2.0','id':index,'method':'tools/call','params':{'name':name,'arguments':arguments,'_meta':{'claudecode/toolUseId':f'room-tool-{index}'}}}}})
                if mode == 'room_stop':
                    aborted = stop_control(False)
                    break
                if mode == 'room_cancel':
                    import time
                    time.sleep(0.1)
                    send({'type':'control_cancel_request','request_id':ident})
                    break
                if mode == 'room_stream_wait':
                    long_stream(session, f'room-drained-{index}')
                reply = read()['response']['response']['mcp_response']['result']
                if mode == 'room_timeout':
                    assert reply['isError'] and 'room_tool_unavailable' in reply['content'][0]['text']
                    break
                assert reply['content'][0]['text'] == ('ROOM_HISTORY_RECEIPT' if index == 0 else 'ROOM_FILE_RECEIPT')
                assert not reply['isError']
        if mode in ('approval', 'cancel', 'hold', 'long_wait'):
            send({'type': 'assistant', 'session_id': session, 'message': {'id': 'step-1', 'content': [{'type': 'tool_use', 'id': 'tool-1', 'name': 'Write', 'input': {'file_path': str(root / 'outside' / 'result')}}], 'usage': {'input_tokens': 10, 'output_tokens': 2, 'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 4}}})
            send({'type': 'control_request', 'request_id': 'approval-1', 'request': {'subtype': 'can_use_tool', 'tool_name': approval_tool, 'tool_use_id': 'tool-1', 'input': {'file_path': str(root / 'outside' / 'result'), 'content': 'exact'}}})
            if mode == 'cancel':
                send({'type': 'control_cancel_request', 'request_id': 'approval-1'})
            else:
                if mode == 'long_wait':
                    long_stream(session, 'stream-drained')
                response = read()['response']['response']
                (root / 'decision').write_text(json.dumps(response))
                if response['behavior'] == 'allow':
                    (root / 'effect').write_text('one')
            send({'type': 'user', 'session_id': session, 'message': {'content': [{'type': 'tool_result', 'tool_use_id': 'tool-1', 'content': 'done'}]}})
        if mode == 'future_turn':
            # A newer CLI mid-turn: a request this host can only refuse, its
            # late withdrawal, a session state it does not know, and extra
            # keys on the permission request it does answer.
            send({'type':'control_request','request_id':'future-1','uuid':'future-envelope-key','request':{'subtype':'future_dialog','detail':{'private':'never shown'}}})
            refusal = read()
            (root / 'refusal').write_text(json.dumps(refusal))
            assert refusal['type'] == 'control_response' and refusal['response']['subtype'] == 'error'
            assert refusal['response']['request_id'] == 'future-1'
            send({'type':'control_cancel_request','request_id':'future-1','reason':'answered'})
            send({'type':'system','subtype':'session_state_changed','state':'future_state','session_id':session})
            send({'type': 'assistant', 'session_id': session, 'message': {'id': 'step-1', 'content': [{'type': 'tool_use', 'id': 'tool-1', 'name': 'Write', 'input': {'file_path': str(root / 'outside' / 'result')}}], 'usage': {'input_tokens': 10, 'output_tokens': 2, 'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 4}}})
            send({'type': 'control_request', 'request_id': 'approval-1', 'uuid': 'future-envelope-key', 'session_id': session, 'request': {'subtype': 'can_use_tool', 'tool_name': 'Write', 'tool_use_id': 'tool-1', 'input': {'file_path': str(root / 'outside' / 'result'), 'content': 'exact'}}})
            response = read()['response']
            assert response['request_id'] == 'approval-1'
            (root / 'decision').write_text(json.dumps(response['response']))
            if response['response']['behavior'] == 'allow':
                (root / 'effect').write_text('one')
            send({'type': 'user', 'session_id': session, 'message': {'content': [{'type': 'tool_result', 'tool_use_id': 'tool-1', 'content': 'done'}]}})
        if mode in ('queued_cancel', 'queued_cancel_question', 'room_queued_cancel'):
            # Two parallel native callbacks. The second waits behind the first
            # owner decision and the CLI withdraws it before that decision.
            send({'type': 'assistant', 'session_id': session, 'message': {'id': 'step-1', 'content': [
                {'type': 'tool_use', 'id': 'tool-1', 'name': 'Write', 'input': {'file_path': str(root / 'outside' / 'result')}},
                {'type': 'tool_use', 'id': 'tool-2', 'name': 'Queued', 'input': {}}],
                'usage': {'input_tokens': 10, 'output_tokens': 2, 'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 4}}})
            send({'type': 'control_request', 'request_id': 'approval-1', 'request': {'subtype': 'can_use_tool', 'tool_name': 'Write', 'tool_use_id': 'tool-1', 'input': {'file_path': str(root / 'outside' / 'result'), 'content': 'exact'}}})
            if mode == 'room_queued_cancel':
                queued = {'subtype':'mcp_message','server_name':'hagency_matrix','message':{'jsonrpc':'2.0','id':7,'method':'tools/call','params':{'name':'read_room_history','arguments':{'cursor':None},'_meta':{'claudecode/toolUseId':'tool-2'}}}}
            elif mode == 'queued_cancel_question':
                queued = {'subtype':'can_use_tool','tool_name':'AskUserQuestion','tool_use_id':'tool-2','input':{'questions':[{'question':'Continue?','header':'Continue','multiSelect':False,'options':[{'label':'Yes','description':'Continue'},{'label':'No','description':'Stop'}]}]}}
            else:
                queued = {'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-2','input':{'command':'never-presented'}}
            send({'type':'control_request','request_id':'queued','request':queued})
            send({'type':'control_cancel_request','request_id':'queued'})
            # Ordered witness: once this text is live the host has read the cancel.
            send({'type':'stream_event','session_id':session,'event':{'type':'message_start','message':{'id':'witness'}}})
            send({'type':'stream_event','session_id':session,'event':{'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'queued callback withdrawn'}}})
            answered = read()['response']
            assert answered['request_id'] == 'approval-1'
            (root / 'decision').write_text(json.dumps(answered['response']))
            if answered['response']['behavior'] == 'allow':
                (root / 'effect').write_text('one')
            send({'type': 'user', 'session_id': session, 'message': {'content': [{'type': 'tool_result', 'tool_use_id': 'tool-1', 'content': 'done'}]}})
        if mode in ('questions', 'question_cancel'):
            questions = [{'question':f'Question {i}?','header':f'Q{i}','multiSelect':i == 0,
                'options':[{'label':'One','description':'First'},{'label':'Two','description':'Second'}]} for i in range(4)]
            send({'type':'control_request','request_id':'question-1','request':{'subtype':'can_use_tool','tool_name':'AskUserQuestion','tool_use_id':'question-tool','input':{'questions':questions}}})
            if mode == 'question_cancel':
                import time
                time.sleep(0.1)
                send({'type':'control_cancel_request','request_id':'question-1'})
            else:
                response = read()['response']['response']
                (root / 'question-answer').write_text(json.dumps(response))
        usage = {} if mode in ('unknown', 'long_stream_unknown') else {'input_tokens': 10, 'output_tokens': 7, 'cache_read_input_tokens': 3, 'cache_creation_input_tokens': 4}
        failed = mode in ('failed', 'failed_exit')
        result = {'type': 'result', 'subtype': 'error' if failed else 'success', 'is_error': failed, 'result': 'offline reply', 'session_id': session, 'usage': usage, 'user_message_uuid':prompt.get('uuid')}
        if aborted and 'success' not in mode:
            result.update({'is_error':True,'terminal_reason':'error_during_execution' if 'generic' in mode else 'aborted_streaming'})
        if mode == 'steer_injected' or mode.startswith('steer_stop'):
            result['user_message_uuids'] = [prompt['uuid']] + ([] if aborted else steers)
        if mode == 'steer_cycle':
            result.update({'result':'steered reply','user_message_uuid':steers[0],'user_message_uuids':steers,
                'modelUsage':{'main':{'inputTokens':20,'outputTokens':14,'cacheReadInputTokens':6,'cacheCreationInputTokens':8}}})
        if mode == 'overflow_usage':
            result['modelUsage'] = {'main': {'inputTokens':9007199254740991,'outputTokens':1,'cacheReadInputTokens':0,'cacheCreationInputTokens':0}}
        if mode == 'long_usage':
            result['modelUsage'] = {'main':{'inputTokens':12000,'outputTokens':1200,'cacheReadInputTokens':0,'cacheCreationInputTokens':0}}
        if continuous or mode in ('cumulative', 'mixed_usage', 'regressing_usage', 'wrong_session', 'background_usage', 'background_wrong_snapshot', 'background_approval', 'background_regressing', 'background_snapshot_eof', 'background_native_followup', 'background_stop_after_answer'):
            count_file = root / 'count'
            count = int(count_file.read_text()) + 1 if count_file.exists() else 1
            count_file.write_text(str(count))
            if mode != 'mixed_usage' or count > 1:
                n = 0 if mode == 'regressing_usage' and process_count > 1 else process_count
                result['modelUsage'] = {'main': {'inputTokens': 10*n, 'outputTokens': 7*n, 'cacheReadInputTokens': 3*n, 'cacheCreationInputTokens': 4*n}, 'subagent': {'inputTokens': 20*n, 'outputTokens': 10*n, 'cacheReadInputTokens': 0, 'cacheCreationInputTokens': 0}}
                if mode.startswith('background_'):
                    # Earlier turns' completed background output remains in
                    # the native cumulative report before this child finishes.
                    result['modelUsage']['subagent']['outputTokens'] = 20*(n-1)+10
        if mode.startswith('background_'):
            child_task = {'task_id':'child-one','task_type':'local_agent'}
            if mode == 'background_native_followup': child_task['run_id'] = 'native-child-run'
            send({'type':'system','subtype':'background_tasks_changed','session_id':session,'tasks':[child_task]})
        send(result)
        if mode == 'steer_cycle':
            lifecycle(steers[0], 'completed')
        if mode.endswith('_exit') and not mode.startswith('background_'):
            # The answer is written; the process ends before native idle.
            sys.exit(0)
        if mode.startswith('background_'):
            if mode == 'background_exit':
                # The CLI dies while its background child is still running.
                sys.exit(0)
            if mode == 'background_fatal_frame':
                print('{"type":"assistant","session_id":', flush=True)
                sys.stdin.read()
                sys.exit(0)
            if mode == 'background_oversized':
                # One child tool result over the host's 1 MiB frame bound.
                send({'type':'user','session_id':session,'parent_tool_use_id':'child-tool','message':{'content':[{'type':'tool_result','tool_use_id':'child-tool','content':'x' * (1024 * 1024 + 1)}]}})
                sys.stdin.read()
                sys.exit(0)
            if mode in ('background_approval', 'background_stop_after_answer'):
                send({'type':'control_request','request_id':'child-approval','request':{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'offline-child-command'}}})
                if mode == 'background_stop_after_answer':
                    # The owner stops the run instead of answering the child.
                    stop_control(False)
                else:
                    response = read()['response']['response']
                    if response['behavior'] == 'allow':
                        (root / 'child-effect').write_text('one')
            if mode != 'background_native_followup':
                send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':session})
            import time
            time.sleep(0.05)
            (root / 'background-completed').write_text(str(count))
            send({'type':'system','subtype':'task_notification','session_id':session,**child_task,'status':'completed'})
            send({'type':'system','subtype':'background_tasks_changed','session_id':session,'tasks':[]})
            if mode == 'background_native_followup':
                send({'type':'system','subtype':'init','session_id':session})
                result['modelUsage']['subagent']['outputTokens'] = 20 * process_count
                result['result'] = 'native background final answer'
                result.pop('user_message_uuid', None)
                result['origin'] = {'kind':'task-notification','producer':'session-task','runId':'native-child-run'}
                send(result)
                send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':session})
                break
            snapshot = read()
            assert snapshot['message']['content'] == '/usage'
            assert snapshot['session_id'] == session
            if mode == 'background_snapshot_eof':
                sys.exit(0)
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':session})
            send({'type':'system','subtype':'init','session_id':session,'user_message_uuids':[snapshot['uuid']]})
            result['modelUsage']['subagent']['outputTokens'] = 20 * process_count
            if mode == 'background_regressing':
                result['modelUsage'] = {'main': {'inputTokens': 0, 'outputTokens': 0, 'cacheReadInputTokens': 0, 'cacheCreationInputTokens': 0}}
            result['result'] = 'local accounting report'
            result['local_command'] = 'usage'
            result['num_turns'] = 0
            result['user_message_uuid'] = 'wrong-call' if mode == 'background_wrong_snapshot' else snapshot['uuid']
            send(result)
        if os.environ.get('CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS') == '1':
            send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':session})
        if not continuous or mode == 'exit':
            break
        if mode == 'late_callback':
            send({'type':'control_request','request_id':'late-callback','request':{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'never-authorize-idle'}}})

else:
    def answer(request, value):
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': value})
    def notify(method, params):
        send({'jsonrpc': '2.0', 'method': method, 'params': params})
    request = read()
    features = ['projection.envelope.v2', 'approval.typed.v1', 'session.workspace_cwd.v1', 'user_question.v1']
    if mode != 'octos_scope_legacy':
        features.append('runtime.host_scoped_profile.v1')
    if mode != 'octos_steer_legacy':
        features.append('turn.steer_active_only.v1')
        features.append('event.turn_steer_dropped.v1')
    answer(request, {'type': 'server_hello', 'capabilities': {'version': {'protocol': 'octos-ui/v1alpha1'}, 'supported_features': features}})
    request = read()
    answer(request, {'current': {'mode': 'read_only', 'network': 'deny'}})
    request = read()
    session = request['params']['session_id']
    answer(request, {'opened': {'session_id': session, 'workspace_root': request['params']['cwd'], 'active_profile_id': 'coding'}})
    request = read()
    if request['method'] == 'peer/tools/register':
        tools = request['params']['tools']
        assert request['params']['generic_tools'] == ['ask_user_question', 'discover_tools', 'run_tool']
        (root / 'room-tools').write_text(json.dumps(tools))
        answer(request, {'session_id':session, 'tools':[{'name':tool['name'], 'model_name':tool['name'].replace('.', '_')} for tool in tools], 'generic_tools':request['params']['generic_tools'], 'applies':'next_turn'})
        request = read()
    turn = request['params']['turn_id']
    if mode in ('octos_files', 'octos_files_revoke', 'octos_files_cancel', 'octos_files_traversal', 'octos_room', 'octos_progress'):
        (root / 'turn-input').write_text(request['params']['input'][0]['text'])
    answer(request, {'accepted': True})
    seq = 0
    def envelope(tag, data):
        global seq
        seq += 1
        notify('projection/envelope', {'session_id': session, 'thread_id': 'main', 'seq': seq, 'turn_id': turn, 'payload': {'type': tag, 'data': data}})
    interrupted = False
    if mode == 'octos_progress':
        assert any(tool['name'] == 'hagency.report_progress' for tool in tools)
        for call_id, text in [('progress-1', 'first finding'), ('progress-2', 'second finding')]:
            notify('peer/tool/call', {'session_id':session, 'turn_id':turn, 'call_id':call_id,
                'name':'hagency.report_progress', 'args':{'text':text}})
            reply = read()
            assert reply['method'] == 'peer/tool/result' and reply['params']['call_id'] == call_id
            assert reply['params']['ok'] is True and reply['params']['data']['delivery'] == 'queued'
            answer(reply, {'call_id':call_id, 'accepted':True})
    if mode in ('octos_question', 'octos_question_steer', 'octos_question_steer_drop', 'octos_question_two_steers', 'octos_question_stop', 'octos_steer_legacy', 'octos_question_cancel'):
        notify('user_question/requested', {'session_id':session, 'question_id':'question-1', 'turn_id':turn,
            'title':'Choose', 'body':'Choose an answer', 'questions':[
                {'header':'First','question':'Which options?','multi_select':True,'allow_free_text':True,
                 'options':[{'label':'One','description':'First'},{'label':'Two','description':'Second'}]},
                {'header':'Second','question':'What else?','multi_select':False,'allow_free_text':True,
                 'options':[{'label':'Yes','description':'Yes'},{'label':'No','description':'No'}]}]})
        if mode == 'octos_question_cancel':
            notify('user_question/cancelled', {'session_id':session, 'question_id':'question-1', 'turn_id':turn, 'reason':'waiter_dropped'})
        else:
            request = read()
            if mode == 'octos_question_stop':
                assert request['method'] == 'turn/interrupt' and request['params']['turn_id'] == turn
                answer(request, {'interrupted':True})
                notify('user_question/cancelled', {'session_id':session, 'question_id':'question-1', 'turn_id':turn, 'reason':'turn_interrupted'})
                interrupted = True
            else:
                if mode in ('octos_question_steer', 'octos_question_steer_drop', 'octos_question_two_steers'):
                    for _ in range(2 if mode == 'octos_question_two_steers' else 1):
                        assert request['method'] == 'turn/steer' and request['params']['require_active'] is True
                        assert request['params']['expected_turn_id'] == turn
                        (root / 'steer').write_text(json.dumps(request['params']))
                        answer(request, {'turn_id':turn, 'steered':True})
                        request = read()
                assert request['method'] == 'user_question/respond' and request['params']['question_id'] == 'question-1'
                (root / 'question-answer').write_text(json.dumps(request['params']['answers']))
                answer(request, {'question_id':'question-1', 'accepted':True, 'runtime_resumed':True})
                if mode in ('octos_question_steer_drop', 'octos_question_two_steers'):
                    notify('turn/steer_dropped', {'session_id':session, 'turn_id':turn,
                        'inputs':['also test'], 'reason':'turn_ended'})
    if mode == 'octos_room_cancel':
        notify('peer/tool/call', {'session_id':session, 'turn_id':turn, 'call_id':'room-cancel', 'name':'hagency.read_room_history', 'args':{'cursor':None}})
        deadline = time.monotonic() + 5
        while not (root / 'room-ready').exists() and time.monotonic() < deadline:
            time.sleep(.01)
        notify('peer/tool/cancel', {'call_id':'room-cancel', 'reason':'turn_interrupted'})
    if mode == 'octos_room':
        for call_id, name, args in [('room-1','hagency.read_room_history',{'cursor':None}), ('room-2','hagency.list_received_files',{})]:
            notify('peer/tool/call', {'session_id':session, 'turn_id':turn, 'call_id':call_id, 'name':name, 'args':args})
            request = read()
            assert request['method'] == 'peer/tool/result' and request['params']['call_id'] == call_id
            with (root / 'room-results').open('a') as result:
                result.write(json.dumps(request['params']) + '\n')
            answer(request, {'call_id':call_id, 'accepted':True})
    if mode in ('octos_files', 'octos_files_revoke', 'octos_files_cancel', 'octos_files_traversal'):
        path = '../outside/escape.txt' if mode == 'octos_files_traversal' else 'plan.txt'
        notify('peer/tool/call', {'session_id':session, 'turn_id':turn, 'call_id':'file-1',
            'name':'hagency.file_create', 'args':{'path':path, 'content':'owner-approved code'}})
        if mode == 'octos_files_cancel':
            deadline = time.monotonic() + 5
            while not (root / 'file-ready').exists() and time.monotonic() < deadline:
                time.sleep(.01)
            notify('peer/tool/cancel', {'call_id':'file-1', 'reason':'turn_interrupted'})
        else:
            request = read()
            assert request['method'] == 'peer/tool/result' and request['params']['call_id'] == 'file-1'
            (root / 'file-result').write_text(json.dumps(request['params']))
            answer(request, {'call_id':'file-1', 'accepted':True})
    if mode in ('approval', 'cancel', 'hold'):
        envelope('tool_start', {'tool_call_id': 'tool-1', 'name': 'shell'})
        notify('approval/requested', {'session_id': session, 'approval_id': 'approval-1', 'turn_id': turn, 'tool_name': 'shell', 'title': 'Run a command', 'body': 'offline', 'approval_kind': 'command', 'risk': 'high', 'typed_details': {'kind': 'command', 'command': {'command_line': 'offline', 'cwd': str(root / 'outside'), 'tool_call_id': 'tool-1'}}})
        if mode == 'cancel':
            notify('approval/cancelled', {'session_id': session, 'approval_id': 'approval-1'})
        else:
            response = read()
            (root / 'decision').write_text(json.dumps(response['params']))
            if response['params']['decision'] == 'approve':
                (root / 'effect').write_text('one')
            answer(response, {'accepted': True})
        envelope('tool_end', {'tool_call_id': 'tool-1'})
    if mode == 'octos_long_stream':
        for _ in range(20000):
            envelope('assistant_delta', {'text':'a', 'assistant_segment_id':'segment-1'})
    elif mode.startswith('octos_'):
        envelope('assistant_delta', {'text':'offline ', 'assistant_segment_id':'segment-1'})
        envelope('assistant_delta', {'text':'reply', 'assistant_segment_id':'segment-1'})
    envelope('assistant_persisted', {'text': 'offline reply', 'assistant_segment_id':'segment-1'})
    tokens = {} if mode in ('unknown', 'octos_missing_with_totals') else {'token_usage': {'input_tokens': 10, 'output_tokens': 7, 'cache_read_tokens': 3, 'cache_write_tokens': 4, 'reasoning_tokens': 2}}
    envelope('turn_terminal', {'outcome': 'interrupted' if interrupted else ('errored' if mode == 'failed' else 'completed'), **tokens})
    notify('session/orchestration', {'session_id': session, 'active': False})
    request = read()
    count_file = root / 'count'
    count = int(count_file.read_text()) + 1 if count_file.exists() else 1
    count_file.write_text(str(count))
    totals = {} if mode == 'unknown' or (mode == 'mixed_usage' and count == 1) else {'input_tokens': 10 * count, 'output_tokens': 7 * count, 'cached_input_tokens': 3 * count, 'cache_write_input_tokens': 4 * count}
    answer(request, {'usage': totals})
# Keep the pipe alive until the host drops/stops the original child.
sys.stdin.read()
