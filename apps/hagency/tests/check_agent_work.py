#!/usr/bin/env python3
"""Evaluate the actual read-only work/history subtrees with the pinned VM.

These are model/control checks, not native pixels or simulated execution proof.
"""
import argparse
import json
from pathlib import Path
import subprocess
from check_agent_admission import PREFIX, APP


def main():
    p = argparse.ArgumentParser()
    p.add_argument('vm', type=Path)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    src = (APP / 'bundle/main.splash').read_text()
    helpers = src[src.index('A.cleanup_enabled = fn('):src.index('A.setup_enabled = fn(')]
    start = src.index('        if H.page == "agent_work_list" {')
    pages = src[start:src.index('        if H.page == "project_wizard"', start)]
    fixture = '''
mod.rinx_icons.inbox=5
mod.rinx_icons.activity=6
A.tokens=fn(value){return value+""}
H.session={features:["shared-work-history-v1"]}
H.page="agent_work" H.agent_id="request" H.agent_selected=agent
H.agent_work=nil H.work_after=nil H.work_page=1
H.work_list=nil H.work_list_after=nil H.work_list_project=nil H.work_list_page=1 H.work_context=nil H.work_from_list=false
A.navigate=fn(page){H.page=page}
let row={id:"work_a" executionId:"execution" eventId:"$event:test" requesterMxid:"@manager:test" state:"completed" replyState:"sent" createdAtUtc:"2026-10-16 00:00:00 UTC" startedAtUtc:nil completedAtUtc:"2026-10-16 00:00:01 UTC" usage:nil}
let usage={knownConsumedTokens:17 retainedTokens:nil complete:false periodKey:"2026-10"}
let history={protocol:"hagency-management-v1" requestId:"request" observedAtUtc:"2026-10-16 01:00:00 UTC" work:{items:[row] nextAfter:nil}}
let entry={requestId:"request" projectId:"project" projectName:"Project Alpha" agentName:"Writer"}
let index={protocol:"hagency-management-v1" observedAtUtc:"2026-10-16 01:00:00 UTC" requests:{items:[entry] nextAfter:nil}}
'''
    cases = {
        'old_server': ('H.session={}', '''assert(has(A.t("Not enabled on this server","此服务器尚未启用")))
assert(button("work_refresh").enabled == false)
A.open_agent_work("request",nil,1)
assert(H.calls.len() == 0)'''),
        'loading': ('H.busy=true', 'assert(has(A.t("Reading work records…","正在读取任务记录…")))\nassert(!button("work_refresh").enabled)'),
        'failed': ('', 'assert(has(A.t("History could not be read. Refresh this page to try again.","未能读取历史，请刷新此页重试。")))'),
        'empty': ('H.agent_work=history history.work.items=[]', 'assert(has(A.t("No work records on this page","此页暂无任务记录")))\nassert(has(A.t("No history is not zero usage. Only recorded shared-agent calls appear here.","暂无历史不等于零用量，此处仅显示已记录的共享 Agent 调用。")))'),
        'null_usage': ('H.agent_work=history', 'assert(has(A.t("Usage not yet recorded","用量尚未记录")))\nassert(!has("0 token"))\nassert(has(A.t("Not recorded","尚未记录")))'),
        'partial_usage': ('H.agent_work=history row.usage=usage', 'assert(has("17 token"))\nassert(has(A.t("Unknown","未知")))\nassert(has(A.t("Partial usage · more accounting is required","用量记录不完整 · 仍需核算")))\nassert(has(A.t("Response completed","响应已完成")))'),
        'complete_spent': ('H.agent_work=history row.usage=usage usage.retainedTokens=17 usage.complete=true', 'assert(has(A.t("Complete usage record","用量记录完整")))\nassert(has("17 token"))\nassert(!has("0 token"))'),
        'zero_observed': ('H.agent_work=history row.usage=usage usage.knownConsumedTokens=0 usage.retainedTokens=0 usage.complete=true', 'assert(has("0 token"))\nassert(has(A.t("Complete usage record","用量记录完整")))'),
        'reply_unknown': ('H.agent_work=history row.state="unknown" row.replyState="unknown"', 'assert(has(A.t("Execution result unknown","执行结果未知")))\nassert(has(A.t("Reply delivery unknown","回复发送结果未知")))'),
        'queued_no_reply': ('H.agent_work=history row.state="queued" row.replyState=nil', 'assert(has(A.t("Queued","已排队")))\nassert(has(A.t("No delivery record","暂无发送记录")))'),
        'forward_state': ('H.agent_work=history row.state="future_state" row.replyState="future_reply"', 'assert(has(A.t("Recorded state · ","已记录状态 · ")+"future_state"))\nassert(has(A.t("Recorded state · ","已记录状态 · ")+"future_reply"))'),
        'paging': ('H.agent_work=history history.work.nextAfter="work_a"', '''H.command={commandId:"original"}
button("work_next").on_click()
assert(H.calls.len() == 1 && H.calls[0].service == "hagency.agents.work")
assert(H.calls[0].args.id == "request" && H.calls[0].args.after == "work_a" && H.calls[0].args.limit == 16)
assert(H.work_page == 2 && H.agent_work == nil)
H.callback({protocol:"hagency-management-v1" requestId:"foreign"})
assert(H.agent_work == nil && H.notice != "")
H.callback(history)
button("work_refresh").on_click()
assert(H.calls[1].args.after == "work_a" && H.calls[1].args.id == "request")
assert(H.command.commandId == "original")'''),
        'index_old_server': ('H.page="agent_work_list" H.session={}', 'assert(has(A.t("Not enabled on this server","此服务器尚未启用")))\nA.open_work_list(nil,nil,1)\nassert(H.calls.len() == 0)'),
        'index_empty': ('H.page="agent_work_list" H.work_list=index index.requests.items=[]', 'assert(has(A.t("No recorded work in your current scope","当前访问范围内暂无任务记录")))'),
        'index_issuance_off': ('H.page="agent_work_list" H.work_list=index', '''assert(!A.agents_enabled() && A.work_enabled())
assert(has("Writer") && has("Project Alpha"))
button("work_list_open").on_click()
assert(H.page == "agent_work" && H.work_from_list)
assert(H.calls[0].service == "hagency.agents.work" && H.calls[0].args.id == "request")
H.callback(history)
A.work_back()
assert(H.page == "agent_work_list" && H.calls[1].service == "hagency.agents.work.list")
assert(H.calls[1].args.includePoolAgents == false)'''),
        'index_pool': ('H.page="agent_work_list" H.session={features:["shared-work-history-v1","pool-work-history-v1"]} entry.projectId=nil entry.projectName=nil entry.poolId="pool" entry.poolName="Resource pool" H.work_list=index', '''assert(has("Resource pool") && has("Writer"))
button("work_list_open").on_click()
assert(H.calls[0].args.id == "request")
assert(A.work_name() == "Writer · Resource pool")
H.callback(history)
A.work_back()
assert(H.calls[1].args.includePoolAgents == true)'''),
        'index_filter_paging': ('H.page="agent_work_list" H.work_list=index H.work_list_project="project" index.requests.nextAfter="request"', '''H.command={commandId:"original"}
button("work_list_next").on_click()
assert(H.calls[0].service == "hagency.agents.work.list" && H.calls[0].args.projectId == "project" && H.calls[0].args.after == "request" && H.calls[0].args.limit == 16)
assert(H.work_list_page == 2 && H.work_list == nil)
H.callback(index)
button("work_list_first").on_click()
assert(H.calls[1].args.projectId == "project" && H.calls[1].args.after == nil && H.work_list_page == 1)
assert(H.command.commandId == "original")'''),
    }
    results = []
    for language in ['en', 'zh']:
        for name, (state, checks) in cases.items():
            script = PREFIX + helpers + fixture
            if language == 'zh':
                script += 'A.t=fn(en,zh){return zh}\n'
            script += state + '\nlet root=View{' + pages + '}\ncollect(root)\n' + checks
            script += '''
for id in ["execute","work_submit","agent_approve","top_up","remove","publish"] {assert(button(id) == nil)}
true
'''
            path = args.output / f'{name}-{language}.splash'
            path.write_text(script)
            result = subprocess.run([str(args.vm.resolve()), 'eval', str(path)], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            (args.output / f'{name}-{language}.log').write_text(result.stdout)
            results.append({'case': f'{name}-{language}', 'passed': result.returncode == 0})
    # The shipped Overview entry must remain reachable after a clean restart
    # with history discovery alone; no saved agent ID or admission feature.
    start = src.index('            if A.work_enabled() {mod.widgets.HagencyCard{')
    overview = src[start:src.index('            manager_work_tools :=', start)]
    for language in ['en', 'zh']:
        for enabled in [False, True]:
            script = PREFIX + helpers + fixture
            if language == 'zh':
                script += 'A.t=fn(en,zh){return zh}\n'
            script += 'H.agent_id="" H.agent_selected=nil H.page="tools" H.entry_role="manager"\n'
            if not enabled:
                script += 'H.session={}\n'
            script += 'let root=View{' + overview + '}\ncollect(root)\n'
            if enabled:
                script += '''assert(!A.agents_enabled())
button("overview_work").on_click()
assert(H.page == "agent_work_list")
assert(H.calls.len() == 1 && H.calls[0].service == "hagency.agents.work.list")
assert(H.calls[0].args.projectId == nil && H.calls[0].args.after == nil && H.calls[0].args.limit == 16)
'''
            else:
                script += 'assert(button("overview_work") == nil)\n'
            script += 'true\n'
            name = f'overview-restart-{enabled}-{language}'
            path = args.output / f'{name}.splash'
            path.write_text(script)
            result = subprocess.run([str(args.vm.resolve()), 'eval', str(path)], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            (args.output / f'{name}.log').write_text(result.stdout)
            results.append({'case': name, 'passed': result.returncode == 0})
    (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    assert all(r['passed'] for r in results), [r for r in results if not r['passed']]
    print(f'{len(results)} actual work-history subtree/pagination checks passed (EN/CN)')


if __name__ == '__main__':
    main()
