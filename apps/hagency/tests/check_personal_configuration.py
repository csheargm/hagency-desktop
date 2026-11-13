#!/usr/bin/env python3
"""Real Splash callbacks for native-reviewed personal configuration, EN/CN."""
import argparse,json,subprocess
from pathlib import Path
from check_full_bundle import APP,fixture
SETUP=r'''
let H=mod.hagency let A=mod.personal_configuration_actions let p=H.personal_config
A.redraw=fn(){return nil} A.save=fn(){return true}
H.busy=false H.command=nil H.notice="" H.ready=true H.personal_id="agent_a" H.page="personal_agent" H.saved_commands=[]
let calls=[] let replies=[]
host.request=fn(service,args,reply){calls.push({service:service args:args}) replies.push(reply)}
let row={id:"agent_a" displayName:"My agent" state:"active" generation:3 executionDeviceId:"device_a"}
let observation={policyRevision:4 budget:{kind:"tokens" tokens:1000 period:"utc_month"} requests:"ask_owner" highRiskTools:"rules" toolRuleCount:2 directoryRuleCount:1 knownConsumedTokens:17 heldTokens:50 model:{model:"qualified" reasoningEffort:"low" webSearch:false workspaceConfigured:true}}
let detail={protocol:"hagency-personal-settings-v1" agent:row settings:{supported:true online:true cached:true observedAtUtc:"2026-10-20" observation:observation}}
H.personal_detail=detail
let choices={agentId:"agent_a" generation:3 profileDigest:"digest-original" current:{runtime:"codex" model:"qualified" reasoningEffort:"low" webSearch:true} models:[{runtime:"codex" model:"qualified" displayName:"Qualified model" reasoningEfforts:["low","high"]}] workspaceConfigured:true canChooseWorkspace:true}
let receipt={protocol:"hagency-personal-settings-v1" commandId:"original" agentId:"agent_a" saved:true profileDigest:"new-digest"}
let start=fn(){A.open_personal_model() replies[0]({is_ok:true data:choices})}
let send=fn(){A.submit_personal_model() replies[1]({is_ok:true data:{commandId:"original"}})}
let widgets=[] let texts=[]
let visit=fn(n){if !n.is_object(){return} for index key child in n{widgets.push({name:key.to_string() value:child}) if key.to_string()=="text" && child.is_string(){texts.push(child)} visit(child)}}
let find=fn(name){for row in widgets{if row.name==name{return row.value}} return nil}
let count=fn(name){let total=0 for row in widgets{if row.name==name{total=total+1}} return total}
let has=fn(text){for value in texts{if value==text{return true}} return false}
'''
CASES={
'definite_conflict_closes_only_exact_failed_request':'start() send() replies[2]({is_ok:false error:"local_revision_conflict"}) assert(H.command==nil && p.draft==nil && calls[3].service=="hagency.intent.pending") replies[3]({is_ok:true data:{commands:[{commandId:"original" hasReply:true},{commandId:"other" hasReply:true}]}}) assert(calls[4].service=="hagency.intent.acknowledge" && calls[4].args.commandId=="original" && calls.len()==5)',
'definite_refusal_without_saved_intent_never_acknowledges':'start() send() replies[2]({is_ok:false error:"configuration_model_unavailable"}) replies[3]({is_ok:true data:{commands:[]}}) assert(H.command==nil && calls.len()==4)',
'failed_original_recovery_can_close':'A.recover_native({commandId:"original" operation:"personal_agent_rules"}) replies[0]({is_ok:false error:"invalid_personal_rules"}) assert(H.command==nil && calls[1].service=="hagency.intent.pending")',
'uncertain_codes_never_classified_as_safe':'for code in ["management_outcome_unknown","configuration_outcome_unknown","configuration_request_expired","management_scope_changed","invalid_management_arguments","management_saved_command_not_found","configuration_unsupported"]{assert(!A.personal_config_not_applied(code))}',
'pending_original_without_reply_is_never_acknowledged':'start() send() replies[2]({is_ok:false error:"configuration_runtime_busy"}) replies[3]({is_ok:true data:{commands:[{commandId:"original" hasReply:false}]}}) assert(calls.len()==4 && H.saved_commands.len()==1)',

'load_exact_agent':'A.open_personal_model() assert(calls[0].service=="hagency.personal_agents.model.choices" && calls[0].args.to_json()=={id:"agent_a"}.to_json()) replies[0]({is_ok:true data:choices}) assert(p.draft.model=="qualified" && p.draft.reasoningEffort=="low" && p.draft.expectedProfileDigest=="digest-original" && p.draft.workspace=="preserve")',
'wrong_agent_refused':'A.open_personal_model() choices.agentId="other" replies[0]({is_ok:true data:choices}) assert(p.choices==nil && p.draft==nil)',
'wrong_generation_refused':'A.open_personal_model() choices.generation=4 replies[0]({is_ok:true data:choices}) assert(p.choices==nil)',
'late_page_reply_ignored':'A.open_personal_model() H.page="overview" replies[0]({is_ok:true data:choices}) assert(p.choices==nil && H.page=="overview")',
'late_epoch_does_not_unlock':'A.open_personal_model() p.epoch=99 replies[0]({is_ok:true data:choices}) assert(H.busy && p.choices==nil)',
'offline_no_request':'detail.settings.online=false A.open_personal_model() A.submit_personal_native("hagency.personal_agents.rules.edit") assert(calls.len()==0)',
'no_observation_no_request':'detail.settings.observation=nil A.open_personal_model() assert(calls.len()==0)',
'more_than_256_models_refused':'A.open_personal_model() let model=choices.models[0] choices.models=[] for i in 0..257{choices.models.push(model)} replies[0]({is_ok:true data:choices}) assert(p.choices==nil)',
'more_than_16_efforts_refused':'A.open_personal_model() choices.models[0].reasoningEfforts=[] for i in 0..17{choices.models[0].reasoningEfforts.push("low")} replies[0]({is_ok:true data:choices}) assert(p.choices==nil)',
'page_only_16_models':'let model=choices.models[0] choices.models=[] for i in 0..33{choices.models.push({runtime:"codex" model:"m"+i displayName:"Model "+i reasoningEfforts:["low"]})} start() RENDER assert(count("personal_model_choice")==16) find("personal_models_next").on_click() assert(p.offset==16)',
'no_models_honest':'choices.models=[] start() RENDER assert(!A.personal_model_valid() && has(A.t("No models reported","设备未上报可用模型")))',
'current_unqualified_requires_choice':'choices.current.model="stale" start() assert(p.draft.model=="" && !A.personal_model_valid())',
'invented_model_refused':'start() A.personal_model_pick("codex","invented",nil) assert(p.draft.model=="qualified") p.draft.model="invented" A.submit_personal_model() assert(calls.len()==1 && H.command==nil)',
'invented_reasoning_refused':'start() A.personal_model_effort("invented") assert(p.draft.reasoningEffort=="low") p.draft.reasoningEffort="invented" assert(!A.personal_model_valid())',
'selection_requires_explicit_effort':'choices.models.push({runtime:"codex" model:"other" displayName:"Other" reasoningEfforts:["high"]}) start() A.personal_model_pick("codex","other",nil) assert(p.draft.reasoningEffort=="" && !A.personal_model_valid()) A.personal_model_effort("high") assert(A.personal_model_valid())',
'callbacks_web_and_workspace':'start() RENDER find("personal_model_search").on_click() find("personal_workspace_default").on_click() assert(!p.draft.webSearch && p.draft.workspace=="default")',
'absent_workspace_defaults':'choices.workspaceConfigured=false start() RENDER assert(p.draft.workspace=="default" && find("personal_workspace_preserve")==nil) p.draft.workspace="preserve" assert(!A.personal_model_valid())',
'no_script_path_or_credentials':'start() send() assert(calls[2].service=="hagency.personal_agents.model.save") assert(calls[2].args.to_json()=={agentId:"agent_a" expectedGeneration:3 expectedProfileDigest:"digest-original" runtime:"codex" model:"qualified" reasoningEffort:"low" webSearch:true workspace:"preserve" commandId:"original"}.to_json()) assert(H.command.nativeRecover)',
'valid_receipt_ack_original':'start() send() replies[2]({is_ok:true data:receipt}) let refreshed=false for call in calls {if call.service=="hagency.personal_agents.get" && call.args.id=="agent_a" {refreshed=true}} assert(H.command==nil && p.result==receipt && calls[3].service=="hagency.intent.acknowledge" && calls[3].args.commandId=="original" && refreshed)',
'mismatched_receipt_retains_original':'start() send() receipt.agentId="other" replies[2]({is_ok:true data:receipt}) assert(H.command.args.commandId=="original" && H.command.nativeRecover && p.result==nil && calls.len()==3)',
'uncertain_retries_only_original_id':'start() send() replies[2]({is_ok:false error:"management_outcome_unknown"}) A.retry_command() assert(calls[3].service=="hagency.intent.retry" && calls[3].args.to_json()=={commandId:"original"}.to_json()) replies[3]({is_ok:true data:{operation:"personal_agent_model" data:receipt}}) assert(H.command==nil && p.result==receipt)',
'restart_recovery_no_editor':'start() send() H.busy=false A.retry_command() assert(calls[3].service=="hagency.intent.retry")',
'cancel_does_not_ack_saved_native':'start() send() replies[2]({is_ok:false error:"personal_configuration_cancelled"}) assert(H.command==nil && calls[3].service=="hagency.intent.pending" && calls.len()==4)',
'new_command_not_erased_by_old_reply':'start() send() H.command={service:"other" args:{commandId:"different"}} H.busy=true replies[2]({is_ok:true data:receipt}) assert(H.command.args.commandId=="different" && H.busy)',
'saved_native_model_recovery':'A.recover_native({commandId:"original" operation:"personal_agent_model"}) assert(calls[0].service=="hagency.intent.retry") replies[0]({is_ok:true data:{operation:"personal_agent_model" data:receipt}}) assert(p.result==receipt)',
'saved_wrong_operation_no_ack':'A.recover_native({commandId:"original" operation:"personal_agent_model"}) replies[0]({is_ok:true data:{operation:"personal_agent_rules" data:receipt}}) assert(calls.len()==1 && p.result==nil)',
'missing_original_retains_command':'start() send() replies[2]({is_ok:false error:"management_outcome_unknown"}) A.retry_command() replies[3]({is_ok:false error:"management_saved_command_not_found"}) assert(H.command.args.commandId=="original" && p.result==nil)',
'custom_rules_native_args_only':'A.submit_personal_native("hagency.personal_agents.rules.edit") replies[0]({is_ok:true data:{commandId:"original"}}) assert(calls[1].service=="hagency.personal_agents.rules.edit" && calls[1].args.to_json()=={agentId:"agent_a" expectedGeneration:3 commandId:"original"}.to_json())',
'workspace_native_args_only':'A.submit_personal_native("hagency.personal_agents.workspace.prepare") replies[0]({is_ok:true data:{commandId:"original"}}) assert(calls[1].args.to_json()=={agentId:"agent_a" expectedGeneration:3 commandId:"original"}.to_json()) replies[1]({is_ok:true data:{protocol:"hagency-personal-settings-v1" agentId:"agent_a" commandId:"original" workspacePrepared:true}}) assert(H.command==nil && p.result.workspacePrepared)',
'rules_result_accepted':'assert(A.personal_config_result_valid({protocol:"hagency-personal-settings-v1" agentId:"agent_a" commandId:"original" saved:true policyRevision:5},"original","agent_a","personal_agent_rules"))',
'local_device_error_translated':'assert(A.personal_config_reason("personal_execution_device_required")!="personal_execution_device_required")',
'busy_does_not_replace_draft':'start() H.busy=true A.personal_model_pick("codex","qualified",nil) A.submit_personal_model() assert(p.draft.reasoningEffort=="low" && calls.len()==1)',
'same_octos_model_keeps_the_selected_profile':'''choices.current={runtime:"octos" model:"local-model" profileId:"first" reasoningEffort:"none" webSearch:false}
choices.models=[{runtime:"octos" model:"local-model" profileId:"first" displayName:"first · local-model" reasoningEfforts:["none"]},{runtime:"octos" model:"local-model" profileId:"second" displayName:"second · local-model" reasoningEfforts:["none"]}]
start() assert(p.draft.profileId=="first" && A.personal_model_choice().profileId=="first") RENDER
assert(has(A.t("Use profile settings","使用配置中的设置")))
assert(has(A.t("Octos uses the reasoning settings in this profile.","Octos 使用该配置中的推理设置。")))
assert(!A.local_model_changed())
let picks=[] for row in widgets{if row.name=="personal_model_choice" {picks.push(row.value)}}
assert(picks.len()==2) picks[1].on_click()
assert(p.draft.profileId=="second" && A.personal_model_choice().profileId=="second" && A.local_model_changed())
send() assert(calls[2].args.profileId=="second" && calls[2].args.model=="local-model")
assert(calls[2].args.to_json().search("credential")<0)
''',
'octos_without_profile_identifier_is_not_selectable':'''choices.current={runtime:"octos" model:"local-model" reasoningEffort:"none" webSearch:false}
choices.models=[{runtime:"octos" model:"local-model" displayName:"old response" reasoningEfforts:["none"]}]
start() assert(A.personal_model_choice()==nil && !A.personal_model_valid())
A.personal_model_pick("octos","local-model",nil) assert(A.personal_model_choice()==nil)
''',
'octos_summary_uses_current_profile_and_ignores_late_results':'''let first={runtime:"octos" model:"local-model" profileId:"first" displayName:"first · local-model"}
let second={runtime:"octos" model:"local-model" profileId:"second" displayName:"second · local-model"}
A.local_status_load("agent_a") A.local_status_load("agent_a")
replies[2]({is_ok:true data:{models:[first,second] current:{runtime:"octos" model:"local-model" profileId:"second"}}})
assert(A.local_model_name("local-model")=="second · local-model")
assert(A.local_reasoning_label({model:"local-model" reasoningEffort:"none"})==A.t("Use profile settings","使用配置中的设置"))
replies[0]({is_ok:true data:{models:[first,second] current:{runtime:"octos" model:"local-model" profileId:"first"}}})
assert(A.local_model_name("local-model")=="second · local-model")
H.local_model_current=nil assert(A.local_model_name("local-model")=="local-model")
H.local_models=[first] assert(A.local_model_name("local-model")=="local-model")
H.local_model_current={runtime:"octos" model:"local-model" profileId:"removed"} assert(A.local_model_name("local-model")=="local-model")
''',
}
def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
 source=(APP/'bundle/main.splash').read_text();start=source.index('        if H.page=="personal_model"{');end=source.index('        if H.page == "personal_agents" {',start)
 render='\nlet tree=View{\n'+source[start:end]+'\n}\nvisit(tree)\n';results=[]
 for lang in ['en','zh-CN']:
  prefix=fixture(source+'\nmod.personal_configuration_actions=A\n',lang)
  for name,body in CASES.items():
   script=a.output/f'{lang}-{name}.splash';script.write_text(prefix+SETUP+body.replace('RENDER',render)+'\ntrue\n');r=subprocess.run([str(a.vm.resolve()),'eval',str(script)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT);script.with_suffix('.log').write_text(r.stdout);results.append({'case':name,'language':lang,'passed':r.returncode==0})
 (a.output/'report.json').write_text(json.dumps({'checks':results},indent=2)+'\n');print(json.dumps({'passed':sum(x['passed'] for x in results),'total':len(results)}));raise SystemExit(not all(x['passed'] for x in results))
if __name__=='__main__':main()
