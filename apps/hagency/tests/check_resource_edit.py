#!/usr/bin/env python3
"""Exercise the shipped editor and recovery callbacks in the pinned VM.

These checks validate script behavior with explicit fixture replies; native
provider/transaction and actual-window acceptance are separate gates.
"""
import argparse,json,subprocess
from pathlib import Path
from check_full_bundle import fixture,APP
SETUP='''
let H=mod.hagency let A=mod.resource_edit_actions let host=mod.resource_edit_host
A.redraw=fn(){} A.save=fn(){return true} H.busy=false H.command=nil H.ready=true H.page="resource_detail" H.resource_id="resource_original"
H.resource_detail={protocol:"hagency-management-v1" editing:{allowed:true reason:nil} ceiling:{tokens:700 period:"monthly"} choices:[{model:"gpt-5.6-sol" reasoning:"medium"},{model:"gpt-5.6-sol" reasoning:"high"},{model:"other-model" reasoning:nil}] resource:{id:"resource_original" revision:"original_revision" framework:"codex" model:"gpt-5.6-sol" reasoning:"medium" capabilities:["coding"] published:false budget:{remainingTokens:700 pool:{ceiling:700 committed:0 period:"monthly"}} usage:{spent:nil spend_period_key:nil}}}
let sent=[] let callback=nil
host.request=fn(service,args,done){sent.push({service:service args:args}) callback=done}
A.submit=fn(service,args){sent.push({service:service args:args})}
'''
CASES={
'default_only_change':'A.edit_resource_configuration() A.choose_resource_default("edit",true) A.submit_resource_configuration() assert(sent.len()==1 && sent[0].args.executionPolicy.yolo && sent[0].args.profile.kind=="preserve" && sent[0].args.ceiling.kind=="preserve")',
'default_existing_preserved':'H.resource_detail.executionPolicy={yolo:true} A.edit_resource_configuration() assert(H.resource_edit_draft.yolo) H.resource_edit_draft.tokens="450" A.submit_resource_configuration() let {executionPolicy=nil}=sent[0].args assert(executionPolicy==nil)',
'default_disable_explicit':'H.resource_detail.executionPolicy={yolo:true} A.edit_resource_configuration() A.choose_resource_default("edit",false) A.submit_resource_configuration() assert(sent.len()==1 && sent[0].args.executionPolicy.yolo==false)',
'default_unsupported_refused':'H.resource_detail.resource.framework="octos" A.edit_resource_configuration() A.choose_resource_default("edit",true) assert(!H.resource_edit_draft.yolo) H.resource_edit_draft.yolo=true A.submit_resource_configuration() assert(sent.len()==0)',
'default_pending_preserved':'A.edit_resource_configuration() H.command={service:"original"} A.choose_resource_default("edit",true) assert(!H.resource_edit_draft.yolo)',
'default_legacy_draft_keeps_policy':'H.resource_detail.executionPolicy={yolo:true} A.edit_resource_configuration() H.resource_edit_draft.yolo=nil H.resource_edit_draft.tokens="450" A.submit_resource_configuration() let {executionPolicy=nil}=sent[0].args assert(executionPolicy==nil)',

'editable_entry':'''A.edit_resource_configuration() assert(H.page=="resource_edit" && H.resource_edit_draft.expectedRevision=="original_revision" && H.resource_edit_draft.tokens=="700" && sent.len()==0)''',
'blocked_readback':'''H.resource_detail.editing.allowed=false A.edit_resource_configuration() assert(H.resource_edit_draft==nil && H.page=="resource_detail" && sent.len()==0)''',
'pending_busy':'''H.busy=true A.edit_resource_configuration() assert(H.resource_edit_draft==nil) H.busy=false H.command={service:"original"} A.edit_resource_configuration() assert(H.resource_edit_draft==nil)''',
'retain_unsent':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="450" H.page="resource_detail" A.edit_resource_configuration() assert(H.resource_edit_draft.tokens=="450")''',
'changed_resource_resets_selection':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="450" H.resource_detail.resource.revision="new_revision" A.edit_resource_configuration() assert(H.resource_edit_draft.tokens=="700" && H.resource_edit_draft.expectedRevision=="new_revision")''',
'zero_limit':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="0" A.submit_resource_configuration() assert(sent.len()==1 && sent[0].service=="hagency.resources.edit" && sent[0].args.resourceId=="resource_original" && sent[0].args.expectedRevision=="original_revision" && sent[0].args.ceiling.kind=="monthly" && sent[0].args.ceiling.tokens==0 && sent[0].args.profile.kind=="preserve")''',
'clear_is_explicit':'''A.edit_resource_configuration() H.resource_edit_draft.limitMode="clear" A.submit_resource_configuration() assert(sent.len()==1 && sent[0].args.ceiling.kind=="clear")''',
'empty_is_not_clear':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="" A.submit_resource_configuration() assert(sent.len()==0 && H.notice!="")''',
'invalid_limits':'''A.edit_resource_configuration() for amount in ["-1","1.5","unknown","9007199254740992"]{H.resource_edit_draft.tokens=amount A.submit_resource_configuration()} assert(sent.len()==0 && H.notice!="")''',
'unchanged_has_no_request':'''A.edit_resource_configuration() A.submit_resource_configuration() assert(sent.len()==0 && H.notice!="")''',
'exact_model_reasoning':'''A.edit_resource_configuration() H.resource_edit_draft.reasoning="high" A.submit_resource_configuration() assert(sent.len()==1 && sent[0].args.profile.kind=="select" && sent[0].args.profile.model=="gpt-5.6-sol" && sent[0].args.profile.reasoning=="high" && sent[0].args.ceiling.kind=="preserve")''',
'model_choices':'''A.edit_resource_configuration() assert(A.resource_edit_models().len()==2) A.choose_resource_model("other-model") assert(H.resource_edit_draft.model=="other-model" && H.resource_edit_draft.reasoning==nil) A.choose_resource_model("invalid") assert(H.resource_edit_draft.model=="other-model")''',
'forged_choice_refused':'''A.edit_resource_configuration() H.resource_edit_draft.model="invalid" A.submit_resource_configuration() assert(sent.len()==0)''',
'stale_snapshot_refused':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="450" H.resource_detail.resource.revision="changed" A.submit_resource_configuration() assert(sent.len()==0 && H.notice!="")''',
'eligibility_lost':'''A.edit_resource_configuration() H.resource_edit_draft.tokens="450" H.resource_detail.editing.allowed=false A.submit_resource_configuration() assert(sent.len()==0)''',
'cancel_preserves_native_recovery':'''let original={service:"hagency.resources.edit" args:{commandId:"original"}} H.command=original A.retry_command() callback({is_ok:false error:"resource_edit_cancelled"}) assert(H.command==nil && !H.busy && sent.len()==2 && sent[1].service=="hagency.intent.pending") callback({is_ok:true data:{commands:[{commandId:"original" operation:"edit_local_resource" hasReply:false}]}}) assert(H.saved_commands.len()==1 && H.saved_commands[0].commandId=="original")''',
'unknown_retains_exact_request':'''let original={service:"hagency.resources.edit" args:{commandId:"original" expectedRevision:"old"}} H.command=original A.retry_command() callback({is_ok:false error:"management_outcome_unknown"}) assert(H.command==original && sent.len()==1 && !H.busy)''',
'changed_first_attempt_settles_only_original':'''H.command={service:"hagency.resources.edit" args:{commandId:"original"}} A.retry_command() callback({is_ok:false error:"management_resource_edit_changed"}) assert(H.command==nil && sent.len()==2 && sent[1].service=="hagency.intent.acknowledge" && sent[1].args.commandId=="original")''',
'saved_result_opens_same_resource':'''let opened="" A.open_resource=fn(id){opened=id} A.acknowledge=fn(id){} H.resource_edit_draft={tokens:"450"} H.command={service:"hagency.resources.edit" args:{commandId:"original"}} A.retry_command() callback({is_ok:true data:{resourceId:"resource_original" revision:"new_revision"}}) assert(H.command==nil && H.resource_edit_draft==nil && opened=="resource_original")''',
'recovered_result_opens_same_resource':'''let opened="" A.open_resource=fn(id){opened=id} A.acknowledge=fn(id){} H.resource_edit_draft={tokens:"450"} A.recover_native({commandId:"original" operation:"edit_local_resource"}) assert(sent[0].service=="hagency.intent.retry") callback({is_ok:true data:{operation:"edit_local_resource" data:{resourceId:"resource_original" revision:"new_revision"}}}) assert(H.resource_edit_draft==nil && opened=="resource_original")''',
'render_editor_choices':'''A.edit_resource_configuration() H.resource_edit_models_open=true let page=View{@@EDITOR@@} let texts=[] let collect=fn(v){if !v.is_object(){return} let {text=""}=v if text!=""{texts.push(text)} for i key child in v {collect(child)}} collect(page) let has=fn(text){for v in texts {if v==text{return true}}return false} assert(has(A.t("Resource configuration","资源配置")) && has(A.t("Review in Hagency","在 Hagency 中确认")) && has("other-model"))'''
}
def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True);source=(APP/'bundle/main.splash').read_text();rows=[]
 for lang in ['en','zh-CN']:
  for name,code in CASES.items():
   editor=source[source.index('        if H.page=="resource_edit" &&'):source.index('        if H.page == "personal_agents" {mod.widgets.HagencyDetailCard{')]
   code=code.replace('@@EDITOR@@',editor)
   path=a.output/f'{lang}-{name}.splash';path.write_text(fixture(source+'\nmod.resource_edit_actions=A\nmod.resource_edit_host=host\n',lang)+SETUP+code+'\ntrue\n')
   r=subprocess.run([str(a.vm.resolve()),'eval',str(path)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT);path.with_suffix('.log').write_text(r.stdout);rows.append({'language':lang,'case':name,'passed':r.returncode==0})
 (a.output/'report.json').write_text(json.dumps(rows,indent=2)+'\n');print(json.dumps({'passed':sum(v['passed'] for v in rows),'total':len(rows),'failed':[v for v in rows if not v['passed']]}));raise SystemExit(not all(v['passed'] for v in rows))
if __name__=='__main__':main()
