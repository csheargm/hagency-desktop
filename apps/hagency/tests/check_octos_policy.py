#!/usr/bin/env python3
"""Exercise the shipped shared Octos permission controls in the Makepad VM."""
import argparse
import json
import subprocess
from pathlib import Path
from check_agent_admission import APP, PREFIX

parser = argparse.ArgumentParser()
parser.add_argument('vm', type=Path)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
source = (APP / 'bundle/main.splash').read_text()
helpers = source[source.index('A.shared_mode = fn('):source.index('// Permission requests are part')]
helpers += source[source.index('A.choose_policy = fn('):source.index('A.valid_policy_result = fn(')]
fixture = '''
H.page="execution_policy" H.policy_resource="pool" H.policy_engagement="agent"
H.policy_mode="detail" H.policy_draft=nil H.calls=[]
let row={resourceId:"pool" engagementId:"agent" agent:"Coder" projectId:"" framework:"octos"
    sharedMode:"standard" webSearch:false executionPolicy:{yolo:false} appliesTo:"next_dispatch"
    revision:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
H.policy_detail=row
assert(A.policy_valid(row,"pool","agent"))
assert(A.policy_auto_available(row))
assert(A.shared_web_tool("octos"))
assert(A.standard_hint("octos")==A.t("Works in its own folder; asks before each command, including interactive terminal sessions","在自己的文件夹内工作；运行命令（包括交互式终端）前会先请求你批准"))
assert(A.policy_choice_label(row,true)==A.t("Automatic approvals","自动批准"))
A.choose_policy(true)
assert(H.policy_draft.executionPolicy.yolo && H.policy_mode=="edit")
A.review_policy() A.submit_policy()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.execution_policy.edit" && H.calls[0].args.executionPolicy.yolo)
row.sharedMode=nil
assert(!A.policy_auto_available(row))
true
'''
records = []
for language in ['en', 'zh']:
    script = args.output / f'{language}.splash'
    script.write_text(PREFIX + helpers + ('A.t=fn(en,zh){return zh}\n' if language == 'zh' else '') + fixture)
    result = subprocess.run([str(args.vm.resolve()), 'eval', str(script)], text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    script.with_suffix('.log').write_text(result.stdout)
    records.append({'language': language, 'passed': result.returncode == 0})
(args.output / 'report.json').write_text(json.dumps(records, indent=2) + '\n')
print(json.dumps({'passed': sum(record['passed'] for record in records), 'total': len(records)}))
raise SystemExit(not all(record['passed'] for record in records))
