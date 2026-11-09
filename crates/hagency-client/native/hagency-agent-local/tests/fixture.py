#!/usr/bin/env python3
# Offline stdio actor for the Desktop bridge; no provider or network calls.
import json, sys, pathlib, os
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

def send(value):
    print(json.dumps(value), flush=True)

def read():
    value = json.loads(sys.stdin.readline())
    with (root / 'frames').open('a') as out:
        out.write(json.dumps(value) + '\n')
    return value

if sys.argv[1:3] == ['auth', 'status']:
    send({'loggedIn': True})
    sys.exit(0)
with (root / 'argv').open('a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\n')

if kind == 'claude':
    init = read()
    if '--resume' in sys.argv and (root / 'missing-context').exists():
        saved = sys.argv[sys.argv.index('--resume') + 1]
        send({'type':'result','session_id':saved,'subtype':'error_during_execution',
            'is_error':True,'num_turns':0,'duration_api_ms':0,'total_cost_usd':0,
            'modelUsage':{},'permission_denials':[],'errors':['No conversation found with session ID: ' + saved],
            'usage':{'input_tokens':0,'output_tokens':0,'cache_creation_input_tokens':0,'cache_read_input_tokens':0}})
        sys.exit(1)
    send({'type': 'control_response', 'response': {'subtype': 'success', 'request_id': init['request_id'], 'response': {}}})
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
    def long_stream(session, marker):
        send({'type':'stream_event','session_id':session,'event':{'type':'message_start','message':{'id':'stream-message'}}})
        for n in range(20_000):
            send({'type':'stream_event','session_id':session,'event':{'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'streamed text '}}})
        (root / marker).write_text('flushed')
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
        session = 'offline-session'
        if os.environ.get('CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS') == '1':
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':session})
            send({'type':'command_lifecycle','command_uuid':'fixture-command','state':'started','session_id':session})
        send({'type': 'system', 'subtype': 'init', 'session_id': session, 'user_message_uuids':[prompt.get('uuid')]})
        aborted = False
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
        if mode.startswith('room'):
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
        result = {'type': 'result', 'subtype': 'error' if mode == 'failed' else 'success', 'is_error': mode == 'failed', 'result': 'offline reply', 'session_id': session, 'usage': usage, 'user_message_uuid':prompt.get('uuid')}
        if aborted:
            result.update({'is_error':True,'terminal_reason':'aborted_streaming'})
        if mode == 'long_usage':
            result['modelUsage'] = {'main':{'inputTokens':12000,'outputTokens':1200,'cacheReadInputTokens':0,'cacheCreationInputTokens':0}}
        if continuous or mode in ('cumulative', 'mixed_usage', 'regressing_usage', 'wrong_session', 'background_usage', 'background_wrong_snapshot', 'background_approval', 'background_regressing', 'background_snapshot_eof'):
            count_file = root / 'count'
            count = int(count_file.read_text()) + 1 if count_file.exists() else 1
            count_file.write_text(str(count))
            if mode != 'mixed_usage' or count > 1:
                n = 0 if mode == 'regressing_usage' and count > 1 else count
                result['modelUsage'] = {'main': {'inputTokens': 10*n, 'outputTokens': 7*n, 'cacheReadInputTokens': 3*n, 'cacheCreationInputTokens': 4*n}, 'subagent': {'inputTokens': 20*n, 'outputTokens': 10*n, 'cacheReadInputTokens': 0, 'cacheCreationInputTokens': 0}}
        if mode.startswith('background_'):
            send({'type':'system','subtype':'background_tasks_changed','session_id':session,'tasks':[{'task_id':'child-one','task_type':'local_agent'}]})
        send(result)
        if mode.startswith('background_'):
            if mode == 'background_approval':
                send({'type':'control_request','request_id':'child-approval','request':{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'offline-child-command'}}})
                response = read()['response']['response']
                if response['behavior'] == 'allow':
                    (root / 'child-effect').write_text('one')
            send({'type':'system','subtype':'session_state_changed','state':'idle','session_id':session})
            import time
            time.sleep(0.05)
            (root / 'background-completed').write_text(str(count))
            send({'type':'system','subtype':'task_notification','session_id':session,'task_id':'child-one','status':'completed'})
            send({'type':'system','subtype':'background_tasks_changed','session_id':session,'tasks':[]})
            snapshot = read()
            assert snapshot['message']['content'] == '/usage'
            assert snapshot['session_id'] == session
            if mode == 'background_snapshot_eof':
                sys.exit(0)
            send({'type':'system','subtype':'session_state_changed','state':'running','session_id':session})
            send({'type':'system','subtype':'init','session_id':session,'user_message_uuids':[snapshot['uuid']]})
            result['modelUsage']['subagent']['outputTokens'] = 20 * count
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
    answer(request, {'type': 'server_hello', 'capabilities': {'version': {'protocol': 'octos-ui/v1alpha1'}}})
    request = read()
    answer(request, {'current': {'mode': 'read_only', 'network': 'deny'}})
    request = read()
    session = request['params']['session_id']
    answer(request, {'opened': {'session_id': session, 'workspace_root': request['params']['cwd'], 'active_profile_id': 'coding'}})
    request = read()
    turn = request['params']['turn_id']
    answer(request, {'accepted': True})
    seq = 0
    def envelope(tag, data):
        global seq
        seq += 1
        notify('projection/envelope', {'session_id': session, 'thread_id': 'main', 'seq': seq, 'turn_id': turn, 'payload': {'type': tag, 'data': data}})
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
    envelope('assistant_persisted', {'text': 'offline reply'})
    tokens = {} if mode == 'unknown' else {'token_usage': {'input_tokens': 10, 'output_tokens': 7, 'cache_read_tokens': 3, 'cache_write_tokens': 4, 'reasoning_tokens': 2}}
    envelope('turn_terminal', {'outcome': 'errored' if mode == 'failed' else 'completed', **tokens})
    notify('session/orchestration', {'session_id': session, 'active': False})
    request = read()
    count_file = root / 'count'
    count = int(count_file.read_text()) + 1 if count_file.exists() else 1
    count_file.write_text(str(count))
    totals = {} if mode == 'unknown' or (mode == 'mixed_usage' and count == 1) else {'input_tokens': 10 * count, 'output_tokens': 7 * count, 'cached_input_tokens': 3 * count, 'cache_write_input_tokens': 4 * count}
    answer(request, {'usage': totals})
# Keep the pipe alive until the host drops/stops the original child.
sys.stdin.read()
