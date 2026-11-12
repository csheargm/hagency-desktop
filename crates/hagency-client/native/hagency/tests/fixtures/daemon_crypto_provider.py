#!/usr/bin/python3
"""Offline Codex protocol peer for the real owner-daemon crypto acceptance test.

No network, credential access, inference, or tools. Every turn is durably marked
in the explicit fixture file so a restart cannot hide duplicate execution.
"""
import json
import os
import sys

if '--version' in sys.argv:
    print('codex-cli 0.0.0-owner-daemon-fixture')
    raise SystemExit(0)

marker = '__FIXTURE_MARKER__'
thread = None
cwd = None
turn_number = 0
for raw in sys.stdin:
    f = json.loads(raw)
    method, ident, p = f.get('method'), f.get('id'), f.get('params', {})
    def reply(v):
        print(json.dumps({'id': ident, 'result': v}), flush=True)
    def emit(m, v):
        print(json.dumps({'method': m, 'params': v}), flush=True)
    if method == 'initialize': reply({})
    elif method == 'initialized': pass
    elif method == 'account/read':
        reply({'requiresOpenaiAuth': True, 'account': {'type': 'chatgpt'}})
    elif method == 'account/rateLimits/read':
        reply({'rateLimits': {'primary': None, 'secondary': None}})
    elif method == 'model/list':
        reply({'data': [{'id': 'fixture', 'model': 'fixture', 'displayName': 'Offline fixture', 'hidden': False,
                        'description': 'Deterministic test only', 'isDefault': True,
                        'defaultReasoningEffort': 'low',
                        'supportedReasoningEfforts': [{'reasoningEffort': 'low', 'description': 'Fixture'}]}],
               'nextCursor': None})
    elif method == 'config/read':
        c = {'project_doc_max_bytes': 0, 'skills': {'include_instructions': False, 'bundled': {'enabled': False}},
             'include_apps_instructions': False, 'include_environment_context': False,
             'developer_instructions': '', 'web_search': 'disabled', 'mcp_servers': {}, 'features': {}}
        for name in ['shell_tool','unified_exec','code_mode_host','code_mode','hooks','plugins','multi_agent','multi_agent_v2',
                     'skill_search','skill_mcp_dependency_install','shell_snapshot','view_image','browser_use',
                     'browser_use_external','browser_use_full_cdp_access','browser_annotation_api','computer_use',
                     'in_app_browser','goals','image_generation','apps','tool_search','tool_suggest','web_search',
                     'web_search_cached','web_search_request','standalone_web_search','memory_tool']:
            c['features'][name] = False
        c['features']['code_mode_host'] = True
        reply({'config': c})
    elif method in ['thread/start', 'thread/resume']:
        thread = p.get('threadId', 'fixture-' + str(os.getpid()))
        cwd = p['cwd']
        reply({'thread': {'id': thread, 'cwd': p['cwd']}, 'cwd': p['cwd'], 'model': p['model'],
               'approvalPolicy': 'untrusted', 'approvalsReviewer': 'user',
               'sandbox': {'type': {'read-only':'readOnly','workspace-write':'workspaceWrite'}[p['sandbox']], 'networkAccess': False}})
    elif method == 'thread/name/set': reply({})
    elif method == 'thread/read': reply({'thread': {'id': thread, 'cwd': cwd, 'status': {'type': 'idle'}}})
    elif method == 'turn/start':
        turn_number += 1
        with open(marker, 'a') as output:
            output.write(json.dumps({'thread': thread, 'input': p.get('input'), 'cwd': cwd}) + '\n')
            output.flush()
            os.fsync(output.fileno())
        turn = 'turn-' + str(os.getpid()) + '-' + str(turn_number)
        reply({'turn': {'id': turn, 'status': 'inProgress'}})
        usage = {'inputTokens': 3, 'outputTokens': 2, 'cachedInputTokens': 0, 'reasoningOutputTokens': 0, 'totalTokens': 5}
        emit('rawResponse/completed', {'threadId': thread, 'turnId': turn, 'responseId': 'response-' + turn, 'usage': usage})
        emit('thread/tokenUsage/updated', {'threadId': thread, 'turnId': turn, 'tokenUsage': {'total': usage, 'last': usage}})
        emit('item/completed', {'threadId': thread, 'turnId': turn, 'item': {'id': 'answer', 'type': 'agentMessage', 'text': 'OFFLINE_ENCRYPTED_DAEMON_ANSWER'}})
        emit('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'completed'}})
    else:
        raise RuntimeError('Unexpected fixture protocol method: ' + str(method))
