#!/usr/bin/env python3
"""Exercise the shipped creation form and original-result callbacks.

Explicit replies are fixtures; actual native transactions and windows have
separate acceptance. A second configuration must never masquerade as an edit.
"""
import argparse, json, subprocess
from pathlib import Path
from check_full_bundle import fixture, APP
from check_resource_edit import SETUP as EDIT_SETUP

SETUP=EDIT_SETUP.replace('mod.resource_edit_actions','mod.resource_create_actions').replace('mod.resource_edit_host','mod.resource_create_host')+'''
H.resource_detail.resource.published=true H.resource_detail.editing={allowed:false reason:"published"}
H.resource_detail.resource.budget.seat={quota:700 remaining:700 committed:0 period:"monthly" status:"declared"}
'''
CASES={
'default_source_inherited':'H.resource_detail.executionPolicy={yolo:true} A.create_resource_configuration() assert(H.resource_create_draft.yolo) A.submit_resource_creation() let {executionPolicy=nil}=sent[0].args assert(executionPolicy==nil && sent[0].service=="hagency.resources.create")',
'default_overridden':'H.resource_detail.executionPolicy={yolo:true} A.create_resource_configuration() A.choose_resource_default("create",false) A.submit_resource_creation() assert(sent.len()==1 && sent[0].args.executionPolicy.yolo==false && H.resource_detail.executionPolicy.yolo)',
'default_enabled':'A.create_resource_configuration() A.choose_resource_default("create",true) A.submit_resource_creation() assert(sent.len()==1 && sent[0].args.executionPolicy.yolo)',
'default_unsupported_refused':'H.resource_detail.resource.framework="octos" A.create_resource_configuration() A.choose_resource_default("create",true) assert(!H.resource_create_draft.yolo) H.resource_create_draft.yolo=true A.submit_resource_creation() assert(sent.len()==0)',

 'published_source':'''A.create_resource_configuration() assert(H.page=="resource_create" && H.resource_create_draft.sourceResourceId=="resource_original" && H.resource_create_draft.tokens=="700" && sent.len()==0 && H.resource_detail.resource.published)''',
 'offered_source':'''H.resource_detail.editing.reason="previously_offered" A.create_resource_configuration() assert(H.page=="resource_create")''',
 'readonly_source':'''H.resource_detail.editing.reason="read_only" A.create_resource_configuration() assert(H.page=="resource_detail" && H.resource_create_draft==nil)''',
 'no_qualified_choice':'''H.resource_detail.choices=[] A.create_resource_configuration() assert(H.resource_create_draft==nil)''',
 'pending_and_busy':'''H.busy=true A.create_resource_configuration() assert(H.resource_create_draft==nil) H.busy=false H.command={service:"original"} A.create_resource_configuration() assert(H.resource_create_draft==nil)''',
 'same_configuration_is_new_request':'''A.create_resource_configuration() A.submit_resource_creation() assert(sent.len()==1 && sent[0].service=="hagency.resources.create" && sent[0].args.sourceResourceId=="resource_original" && sent[0].args.expectedRevision=="original_revision" && sent[0].args.profile.kind=="preserve" && sent[0].args.ceiling.kind=="preserve")''',
 'zero_limit':'''A.create_resource_configuration() H.resource_create_draft.tokens="0" A.submit_resource_creation() assert(sent.len()==1 && sent[0].args.ceiling.kind=="monthly" && sent[0].args.ceiling.tokens==0)''',
 'clear_limit':'''A.create_resource_configuration() H.resource_create_draft.limitMode="clear" A.submit_resource_creation() assert(sent.len()==1 && sent[0].args.ceiling.kind=="clear")''',
 'empty_is_not_clear':'''A.create_resource_configuration() H.resource_create_draft.tokens="" A.submit_resource_creation() assert(sent.len()==0 && H.notice!="")''',
 'invalid_limits':'''A.create_resource_configuration() for amount in ["-1","1.5","unknown","9007199254740992"]{H.resource_create_draft.tokens=amount A.submit_resource_creation()} assert(sent.len()==0)''',
 'exact_model_reasoning':'''A.create_resource_configuration() H.resource_create_draft.reasoning="high" A.submit_resource_creation() assert(sent[0].args.profile.kind=="select" && sent[0].args.profile.model=="gpt-5.6-sol" && sent[0].args.profile.reasoning=="high")''',
 'model_choices':'''A.create_resource_configuration() A.choose_resource_creation_model("other-model") assert(H.resource_create_draft.reasoning==nil) A.choose_resource_creation_model("not-real") assert(H.resource_create_draft.model=="other-model")''',
 'forged_choice':'''A.create_resource_configuration() H.resource_create_draft.model="not-real" A.submit_resource_creation() assert(sent.len()==0)''',
 'separate_retained_drafts':'''H.resource_edit_draft={tokens:"333"} A.create_resource_configuration() H.resource_create_draft.tokens="450" H.page="resource_detail" A.create_resource_configuration() assert(H.resource_create_draft.tokens=="450" && H.resource_edit_draft.tokens=="333" && H.resource_detail.ceiling.tokens==700)''',
 'changed_source_resets_draft':'''A.create_resource_configuration() H.resource_create_draft.tokens="450" H.resource_detail.resource.revision="changed" A.create_resource_configuration() assert(H.resource_create_draft.tokens=="700" && H.resource_create_draft.expectedRevision=="changed")''',
 'stale_source_refused':'''A.create_resource_configuration() H.resource_detail.resource.revision="changed" A.submit_resource_creation() assert(sent.len()==0 && H.notice!="")''',
 'cancel_keeps_native_recovery':'''H.command={service:"hagency.resources.create" args:{commandId:"original" sourceResourceId:"resource_original"}} A.retry_command() callback({is_ok:false error:"resource_create_cancelled"}) assert(H.command==nil && !H.busy && sent.len()==2 && sent[1].service=="hagency.intent.pending") callback({is_ok:true data:{commands:[{commandId:"original" operation:"create_resource_configuration" hasReply:false}]}}) assert(H.saved_commands[0].commandId=="original")''',
 'unknown_retains_original':'''let original={service:"hagency.resources.create" args:{commandId:"original" sourceResourceId:"resource_original"}} H.command=original A.retry_command() callback({is_ok:false error:"management_outcome_unknown"}) assert(H.command==original && sent.len()==1 && !H.busy)''',
 'collision_retains_original':'''let original={service:"hagency.resources.create" args:{commandId:"original"}} H.command=original A.retry_command() callback({is_ok:false error:"management_revision_conflict"}) assert(H.command==original && sent.len()==1)''',
 'source_change_settles_only_original':'''H.command={service:"hagency.resources.create" args:{commandId:"original"}} A.retry_command() callback({is_ok:false error:"management_resource_source_changed"}) assert(H.command==nil && sent.len()==2 && sent[1].service=="hagency.intent.acknowledge" && sent[1].args.commandId=="original")''',
 'success_opens_new_resource':'''let opened="" A.open_resource=fn(id){opened=id} A.acknowledge=fn(id){} H.resource_create_draft={tokens:"450"} H.resource_edit_draft={tokens:"333"} H.command={service:"hagency.resources.create" args:{commandId:"original" sourceResourceId:"resource_original"}} A.retry_command() callback({is_ok:true data:{resourceId:"resource_new" revision:"new_revision"}}) assert(opened=="resource_new" && H.command==nil && H.resource_create_draft==nil && H.resource_edit_draft.tokens=="333")''',
 'recovery_opens_original_new_result':'''let opened="" A.open_resource=fn(id){opened=id} A.acknowledge=fn(id){} H.resource_create_draft={tokens:"450"} A.recover_native({commandId:"original" operation:"create_resource_configuration"}) assert(sent[0].service=="hagency.intent.retry") callback({is_ok:true data:{operation:"create_resource_configuration" data:{resourceId:"resource_new" revision:"original_revision"}}}) assert(opened=="resource_new" && H.resource_create_draft==nil)''',
 'render_source_and_shared_limit':'''A.create_resource_configuration() H.resource_create_models_open=true let page=View{@@FORM@@} let texts=[] let collect=fn(v){if !v.is_object(){return} let {text=""}=v if text!=""{texts.push(text)} for i key child in v {collect(child)}} collect(page) let has=fn(text){for v in texts {if v==text{return true}}return false} assert(has(A.t("Create a new configuration","创建新配置")) && has(A.t("Shared account limit · unchanged","共享账号上限 · 保持不变")) && has("resource_original") && has("other-model"))''',
}

def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
 source=(APP/'bundle/main.splash').read_text();form=source[source.index('        if H.page=="resource_create" &&'):source.index('        if H.page == "personal_agents" {mod.widgets.HagencyDetailCard{')];rows=[]
 for lang in ['en','zh-CN']:
  for name,code in CASES.items():
   path=a.output/f'{lang}-{name}.splash';path.write_text(fixture(source+'\nmod.resource_create_actions=A\nmod.resource_create_host=host\n',lang)+SETUP+code.replace('@@FORM@@',form)+'\ntrue\n')
   r=subprocess.run([str(a.vm.resolve()),'eval',str(path)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT);path.with_suffix('.log').write_text(r.stdout);rows.append({'language':lang,'case':name,'passed':r.returncode==0})
 (a.output/'report.json').write_text(json.dumps(rows,indent=2)+'\n');print(json.dumps({'passed':sum(v['passed'] for v in rows),'total':len(rows),'failed':[v for v in rows if not v['passed']]}));raise SystemExit(not all(v['passed'] for v in rows))
if __name__=='__main__':main()
