#!/usr/bin/env python3
"""Retained settings state during async saves, identity changes and retry failures."""
import argparse, json, subprocess
from pathlib import Path
from check_full_bundle import APP, fixture
SETUP = r'''
A.redraw=fn(){} A.save=fn(){return true}
H.busy=false H.command=nil H.calls=[] H.callbacks=[] H.bar_past=true
A.request=fn(service,args,done){H.calls.push(service) H.callbacks.push(done)}
A.sub_read=fn(service,args,done,fail){H.calls.push(service) H.callbacks.push(done) H.failed=fail return true}
host.request=fn(service,args,done){H.calls.push(service) H.callbacks.push(done)}
'''
CASES = {
"confirmed_default_permissions_update_pool_summary": r'''
H.pool_info={rid:"pool" defaultYolo:false}
H.resource_detail={resource:{id:"pool" framework:"codex"} editing:{allowed:false} executionPolicy:{yolo:false}}
H.dpol.resource="pool" H.dpol.detail=H.resource_detail H.dpol.mode="catalog" H.dpol.state="ready" H.dpol.yolo=false H.dpol.catalog={resourceId:"pool" revision:"before"}
A.web_can_edit=fn(){return true}
A.dpol_choose(true)
assert(H.dpol.ask==true && A.pool_default_yolo(H.resource_detail)==false)
A.dpol_commit()
H.callbacks[0]({is_ok:true data:{commandId:"policy-original" resourceId:"pool" expectedRevision:"before" change:{operation:"default_policy" yolo:true webSearch:false}}})
assert(H.calls[1]=="hagency.web_catalog.command.apply")
H.callbacks[1]({is_ok:true data:{commandId:"policy-original"}})
H.callbacks[2]({is_ok:true data:{}})
assert(H.calls[3]=="hagency.web_catalog.get")
H.callbacks[3]({resourceId:"pool" revision:"after" defaultYolo:true})
assert(!H.busy && H.dpol.yolo && !H.dpol.waiting)
assert(A.pool_default_yolo(H.resource_detail)==true)
assert(!H.resource_detail.executionPolicy.yolo)
''',
"claude_pool_defaults_to_full_trust_and_other_tools_cannot": r'''
H.entry_role="owner" H.page="default_policy"
H.dpol.resource="pool" H.dpol.detail={resource:{id:"pool" framework:"octos"}}
H.dpol.mode="catalog" H.dpol.state="ready" H.dpol.yolo=false H.dpol.web=false H.dpol.catalog={resourceId:"pool" revision:"before"}
A.web_can_edit=fn(){return true}
assert(A.full_trust_tool("codex") && A.full_trust_tool("claude") && !A.full_trust_tool("octos"))
assert(A.full_trust_hint("codex","original")=="original" && A.full_trust_hint("claude","original")!="original")
assert(A.full_trust_hint("octos","original")!="original" && A.full_trust_hint("octos","original")!=A.full_trust_hint("claude","original"))
A.dpol_choose(true)
assert(H.dpol.ask!=true)
H.dpol.detail={resource:{id:"pool" framework:"claude"}}
A.dpol_choose(true)
assert(H.dpol.ask==true)
let prepared=nil
host.request=fn(service,args,done){H.calls.push(service) H.callbacks.push(done) if service=="hagency.web_catalog.prepare"{prepared=args}}
A.dpol_commit()
assert(prepared.change.operation=="default_policy" && prepared.change.yolo && !prepared.change.webSearch && H.busy)
''',
"claude_agent_takes_full_trust_and_other_tools_cannot": r'''
H.page="execution_policy" H.policy_mode="detail" H.policy_resource="pool" H.policy_engagement="agent"
let revision="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
let row={resourceId:"pool" engagementId:"agent" agent:"Reviewer" projectId:"project" framework:"octos" executionPolicy:{yolo:false} appliesTo:"next_dispatch" revision:revision}
H.policy_detail=row H.policy_draft=nil
A.choose_policy(true)
assert(H.policy_draft==nil && H.policy_mode=="detail")
let trusted={resourceId:"pool" engagementId:"agent" agent:"Reviewer" projectId:"project" framework:"octos" executionPolicy:{yolo:true} appliesTo:"next_dispatch" revision:revision}
assert(!A.policy_valid(trusted,"pool","agent"))
trusted.framework="claude"
assert(A.policy_valid(trusted,"pool","agent"))
row.framework="claude"
A.choose_policy(true)
assert(H.policy_mode=="edit" && H.policy_draft.executionPolicy.yolo && H.policy_draft.engagementId=="agent")
A.review_policy()
let sent=nil
A.submit=fn(service,args){sent={service:service args:args}}
A.submit_policy()
assert(sent.service=="hagency.execution_policy.edit" && sent.args.executionPolicy.yolo && sent.args.expectedRevision==revision)
''',
"pooled_claude_agent_keeps_its_web_search_switch": r'''
H.page="execution_policy" H.policy_mode="detail" H.policy_resource="pool" H.policy_engagement="agent"
let revision="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
let row={sharedMode:"standard" webSearch:false resourceId:"pool" engagementId:"agent" agent:"Reviewer" projectId:"" framework:"claude" executionPolicy:{yolo:false} appliesTo:"next_dispatch" revision:revision}
assert(A.policy_valid(row,"pool","agent"))
assert(A.standard_hint("claude")!=A.standard_hint("codex") && A.standard_hint("octos")!=A.standard_hint("claude"))
H.policy_detail=row
let sent=nil
A.submit=fn(service,args){sent={service:service args:args}}
A.toggle_policy_web()
assert(sent.service=="hagency.execution_policy.edit" && sent.args.webSearch==true && !sent.args.executionPolicy.yolo && sent.args.expectedRevision==revision)
let other={sharedMode:"standard" webSearch:false resourceId:"pool" engagementId:"agent" agent:"Reviewer" projectId:"" framework:"octos" executionPolicy:{yolo:false} appliesTo:"next_dispatch" revision:revision}
assert(!A.policy_valid(other,"pool","agent"))
sent=nil H.policy_detail=other
A.toggle_policy_web()
assert(sent==nil)
''',
"resource_web_search_preserves_permission_and_waits_for_matching_readback": r'''
H.entry_role="owner" H.page="default_policy"
H.dpol.resource="pool" H.dpol.detail={resource:{id:"pool" framework:"codex"}}
H.dpol.mode="catalog" H.dpol.state="ready" H.dpol.yolo=true H.dpol.web=false H.dpol.catalog={resourceId:"pool" revision:"before"}
A.web_can_edit=fn(){return true}
let prepared=nil
host.request=fn(service,args,done){H.calls.push(service) H.callbacks.push(done) if service=="hagency.web_catalog.prepare"{prepared=args}}
A.dpol_web()
assert(prepared.change.yolo && prepared.change.webSearch && H.busy && !H.dpol.web)
H.callbacks[0]({is_ok:true data:{commandId:"web-original" resourceId:"pool" expectedRevision:"before" change:prepared.change}})
H.callbacks[1]({is_ok:true data:{commandId:"web-original"}})
H.callbacks[2]({is_ok:true data:{}})
H.callbacks[3]({resourceId:"pool" revision:"before" defaultYolo:true defaultWebSearch:false})
assert(H.dpol.waiting && H.dpol.web && H.dpol.yolo)
A.dpol_catalog(H.dpol.epoch)
H.callbacks[4]({resourceId:"pool" revision:"after" defaultYolo:true defaultWebSearch:true})
assert(!H.busy && !H.dpol.waiting && H.dpol.web && H.dpol.yolo)
''',
"resource_web_search_rejects_cross_resource_preparation": r'''
H.entry_role="owner" H.dpol.resource="pool" H.dpol.detail={resource:{id:"pool" framework:"codex"}}
H.dpol.mode="catalog" H.dpol.state="ready" H.dpol.yolo=false H.dpol.web=false H.dpol.catalog={resourceId:"pool" revision:"before"}
A.web_can_edit=fn(){return true}
A.dpol_web()
H.callbacks[0]({is_ok:true data:{commandId:"foreign" resourceId:"another-pool" expectedRevision:"before" change:{operation:"default_policy" yolo:false webSearch:true}}})
assert(H.calls.len()==1 && !H.busy && H.dpol.error=="management_outcome_unknown" && !H.dpol.web)
''',
"resource_web_search_unknown_apply_retains_original_command": r'''
H.entry_role="owner" H.dpol.resource="pool" H.dpol.detail={resource:{id:"pool" framework:"codex"}}
H.dpol.mode="catalog" H.dpol.state="ready" H.dpol.yolo=false H.dpol.web=false H.dpol.catalog={resourceId:"pool" revision:"before"}
A.web_can_edit=fn(){return true}
let retained=nil A.web_prepared=fn(p){retained=p}
A.dpol_web()
H.callbacks[0]({is_ok:true data:{commandId:"web-original" resourceId:"pool" expectedRevision:"before" change:{operation:"default_policy" yolo:false webSearch:true}}})
H.callbacks[1]({is_ok:false error:"management_outcome_unknown"})
assert(H.calls.len()==2 && retained.commandId=="web-original" && retained.change.webSearch && !H.dpol.web && !H.busy)
''',
"pool_refresh_keeps_content_and_scroll": r'''
H.resource_id="pool" H.resource_detail={resource:{id:"pool"}} H.pool_saved=nil
A.open_resource("pool")
assert(H.resource_detail.resource.id=="pool" && H.bar_past)
let before=timers.len()
A.pool_say("quota","running","Saved")
assert(timers.len()==before)
assert(A.settings_feedback().text=="Saved")
A.settings_feedback_done("pool")
assert(H.pool_result==nil && H.resource_detail!=nil)
A.open_resource("different")
assert(H.resource_detail==nil && !H.bar_past)
''',
"default_refresh_keeps_controls_and_blocks_stale_writes": r'''
H.dpol.resource="pool" H.dpol.detail={resource:{id:"pool"}} H.dpol.mode="resource" H.dpol.state="ready" H.dpol.yolo=true H.dpol.saved=true
A.dpol_open("pool")
assert(H.dpol.detail!=nil && H.dpol.state=="ready" && H.dpol.yolo && H.bar_past)
assert(!A.dpol_can() && A.settings_feedback().tone=="waiting")
H.failed("management_unavailable")
assert(H.dpol.detail!=nil && H.dpol.state=="ready" && !A.dpol_can())
assert(A.settings_feedback().kind=="default_error")
A.dpol_open("different")
assert(H.dpol.detail==nil && H.dpol.state=="loading" && !H.bar_past && H.dpol.saved==nil)
''',
"personal_refresh_keeps_snapshot_but_not_across_agents": r'''
H.personal_id="agent" H.personal_detail={agent:{id:"agent"}} H.local_status={state:"running"} H.local_models=["one"]
A.open_personal_agent("agent")
assert(H.personal_detail.agent.id=="agent" && H.local_status.state=="running" && H.local_models.len()==1)
A.personal_saved({agentId:"agent"})
assert(H.personal_detail.agent.id=="agent" && A.settings_feedback().kind=="personal")
A.personal_observation_received({agentId:"agent"})
assert(H.personal_detail.agent.id=="agent" && A.settings_feedback().tone=="waiting")
A.open_personal_agent("different")
assert(H.personal_detail==nil && H.local_status==nil && H.local_models==nil && H.personal_result==nil)
''',
}
def main():
    p=argparse.ArgumentParser(); p.add_argument("vm",type=Path); p.add_argument("--output",type=Path,required=True); a=p.parse_args(); a.output.mkdir(parents=True,exist_ok=True)
    source=(APP/"bundle/main.splash").read_text(); results=[]
    for language in ("en","zh-CN"):
        for name, case in CASES.items():
            path=a.output/(language+"-"+name+".splash"); path.write_text(fixture(source+SETUP+case,language))
            r=subprocess.run([str(a.vm.resolve()),"root",str(path)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            path.with_suffix(".log").write_text(r.stdout); results.append(dict(case=name,language=language,passed=r.returncode==0))
    (a.output/"report.json").write_text(json.dumps(results,indent=2)+"\n")
    print(json.dumps(dict(passed=sum(r["passed"] for r in results),total=len(results))))
    raise SystemExit(not all(r["passed"] for r in results))
if __name__=="__main__": main()
