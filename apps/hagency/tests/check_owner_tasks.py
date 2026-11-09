#!/usr/bin/env python3
"""Owner task contexts, source identity and native confirmation handoffs."""
import argparse, json, subprocess
from pathlib import Path
from check_full_bundle import APP, fixture
SETUP='''
A.redraw=fn(){} A.save=fn(){return true}
H.ready=true H.busy=false H.command=nil H.calls=[] H.callbacks=[] H.entry_role="owner"
H.session={account:"@owner:test" device:"device" canAdmitResources:false features:["resource-pool-agents-v1"]}
host.request=fn(service,args,done){H.calls.push({service:service args:args}) H.callbacks.push(done)}
let reply=fn(index,data){H.callbacks[index]({is_ok:true data:data})}
let connection={id:"pool_original" label:"Pool A" ownerMxid:"@owner:test" executorDeviceId:"device" state:"admitted"}
'''
CASES={
'current_device_setup_requires_uninitialized_state': '''
let fresh=A.pool_device_state({state:"uninitialized" canReview:true grantReviewAvailable:false grantIntents:[]},"ready")
assert(fresh.fix && fresh.detail==A.t("Set up resource management for this account before connecting a coding tool and creating a pool.","先为当前账号完成资源管理初始化，再接入编码工具、创建资源池。"))
let ready=A.pool_device_state({state:"active" canReview:false grantReviewAvailable:true grantIntents:[]},"ready")
assert(ready.key=="ready" && !ready.fix)
for state in ["active","unavailable","untrusted"] {
 let blocked=A.pool_device_state({state:state canReview:true grantReviewAvailable:false grantIntents:[]},"ready")
 assert(!blocked.fix)
}
''',
'device_setup_preserves_specific_blocker_without_resubmitting': '''
A.pool_device_load=fn(){}
A.pool_call=fn(service,args,done){H.calls.push({service:service args:args}) H.callbacks.push(done)}
A.dev_setup("hub")
reply(0,{state:"uninitialized" canReview:true grantReviewAvailable:false grantIntents:[]})
assert(H.calls.len()==2 && H.calls[1].service=="hagency.onboarding.review")
reply(1,{state:"outcome_unknown" reason:"source_installation_busy"})
assert(H.calls.len()==2 && H.dev.result.tone=="attention" && H.dev.result.text==A.dev_setup_error("source_installation_busy"))
''',
'device_setup_keeps_uncertain_result_when_no_reason_arrives': '''
A.pool_device_load=fn(){}
A.pool_call=fn(service,args,done){H.calls.push({service:service args:args}) H.callbacks.push(done)}
A.dev_setup("hub")
reply(0,{state:"uninitialized" canReview:true grantReviewAvailable:false grantIntents:[]})
reply(1,{state:"outcome_unknown"})
assert(H.calls.len()==2 && H.dev.result.tone=="waiting")
assert(H.dev.result.text==A.t("It isn’t known yet whether setup finished. Tap Continue Setting Up again in a moment; it won’t run twice.","暂时不知道设置是否完成。稍后再点「继续设置这台电脑」查看，不会重复设置。"))
''',
'whitelist_preserves_verified_picks_in_native_confirmation': '''
H.resource_detail={resource:{budget:{pool:{ceiling:2000000 committed:0 period:"monthly"} remainingTokens:2000000 seat:{quota:nil}} usage:{spent:0}}} H.page="pool_who" H.resource_id="resource_a"
H.who={rid:"resource_a" name:"" picks:["@chris:test"]}
A.pool_view=fn(r){return {quotaEditable:false quotaMode:"amend" publishedTokens:1000000 b:{ceiling:2000000} now:1793400000000}}
H.pool_info={rid:"resource_a" conn:connection grantState:"ready" grant:nil now:1793400000000}
A.pool_effective_grant=fn(c,g,now){return nil}
A.pool_floor=fn(v){return nil}
A.pool_call=fn(service,args,done){H.calls.push({service:service args:args})}
A.who_submit()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.grants.review")
assert(H.calls[0].args.contributionId=="pool_original" && H.calls[0].args.choosePeople)
assert(H.calls[0].args.applicants.len()==1 && H.calls[0].args.applicants[0]=="@chris:test")
assert(H.who.picks.len()==1 && H.who.picks[0]=="@chris:test")
''',
'million_display_is_exact_not_an_authority_conversion': '''
assert(A.tokens(0)=="0 M" && A.tokens(1)=="0.000001 M")
assert(A.tokens(750000)=="0.75 M" && A.tokens(1000000)=="1 M")
assert(A.tokens(123456)=="0.123456 M" && A.tokens(1000001)=="1.000001 M")
assert(A.tokens(9007199254740991)=="9007199254.740991 M")
assert(A.tokens(nil)==A.t("Unknown","未知") && A.amount(1000000)=="1,000,000")
''',
'allowance_summaries_are_compact_without_hiding_small_positive_usage': '''
assert(A.token_summary(39994936)=="39.99 M" && A.token_summary(40000000)=="40 M")
assert(A.token_summary(1235000)=="1.24 M" && A.token_summary(1250000)=="1.25 M")
assert(A.token_summary(1500000)=="1.5 M" && A.token_summary(999999)=="1 M")
assert(A.token_summary(0)=="0 M" && A.token_summary(1)=="<0.01 M" && A.token_summary(9999)=="<0.01 M")
assert(A.token_summary(10000)=="0.01 M" && A.token_summary(nil)==A.t("Unknown","未知"))
assert(A.tokens(39994936)=="39.994936 M" && A.amount(39994936)=="39,994,936")
assert(H.calls.len()==0)
''',

'creation_continuation_keeps_exact_unsent_values': '''
H.enrollment_draft={account:{accountId:"login_a" expectedRevision:"original"} model:{model:"chosen"} tokens:"445678" reasoning:"high"}
let saved=H.enrollment_draft.to_json()
H.page="resources" A.owner_continue_pool()
assert(H.page=="account_enrollment" && H.enrollment_draft.to_json()==saved && H.calls.len()==0)
H.owner.creatingPool=true A.open_account("login_a",nil)
assert(H.owner.accountTab=="pools" && !H.owner.creatingPool && H.enrollment_draft.to_json()==saved)
''',

'automatic_reads_keep_task_context': '''
H.page="resources" H.owner.accountId="login_a" H.ux.resourceCursor="page_2"
let plan=A.web_refresh_plan()
assert(plan.service=="hagency.accounts.get" && plan.args.id=="login_a" && plan.args.after=="page_2")
plan.done({protocol:"hagency-management-v1" account:{id:"login_other"} resources:[{id:"wrong"}] nextAfter:nil})
assert(H.resources.len()==0)
H.owner.accountId="" H.owner.poolsReady=false
plan=A.web_refresh_plan() assert(plan.service=="hagency.resources.list")
plan.done({resources:[] nextAfter:nil}) assert(H.owner.poolsReady)
H.page="inbox" H.owner.poolId="pool_original" H.reqs.items=[] H.reqs.state="ready" H.reqs.next="request_page_2"
assert(A.web_refresh_plan()==nil)
H.reqs.next=nil
plan=A.web_refresh_plan()
assert(plan.service=="hagency.pools.agents.list" && plan.args.poolId=="pool_original" && plan.args.after==nil)
plan.done({items:[] nextAfter:nil}) assert(H.reqs.items.len()==0 && H.reqs.next==nil)
''',

'account_scope_and_pagination': '''
// Pool status is fetched separately; this case exercises the account page.
A.pool_meta_load=fn(){}
H.page="resources" H.owner.accountId="login_a" H.ux.resourceCursor=nil
A.owner_load_pools()
assert(H.calls[0].service=="hagency.accounts.get" && H.calls[0].args.id=="login_a")
reply(0,{protocol:"hagency-management-v1" account:{id:"login_a"} resources:[{id:"resource_a"}] nextAfter:"cursor_a"})
assert(H.owner.poolsReady && H.resources[0].id=="resource_a" && H.resource_after=="cursor_a")
A.more()
assert(H.calls[1].args.id=="login_a" && H.calls[1].args.after=="cursor_a")
reply(1,{protocol:"hagency-management-v1" account:{id:"login_a"} resources:[{id:"resource_b"}] nextAfter:nil})
assert(H.resources[0].id=="resource_b" && H.resource_after==nil)
''',
'stale_or_mismatched_pool_reply': '''
H.page="resources" H.owner.accountId="login_a" A.owner_load_pools()
H.owner.accountId="login_b"
reply(0,{protocol:"hagency-management-v1" account:{id:"login_a"} resources:[{id:"wrong"}] nextAfter:nil})
assert(!H.owner.poolsReady && H.resources.len()==0)
A.owner_load_pools()
reply(1,{protocol:"hagency-management-v1" account:{id:"login_a"} resources:[{id:"wrong"}] nextAfter:nil})
assert(!H.owner.poolsReady && H.resources.len()==0 && H.notice!="")
''',
'all_pools_uses_native_inventory': '''
H.page="resources" H.owner.accountId="" H.ux.resourceCursor="cursor" A.owner_load_pools()
assert(H.calls[0].service=="hagency.resources.list" && H.calls[0].args.after=="cursor")
reply(0,{resources:[{id:"native"}] nextAfter:nil})
assert(H.owner.poolsReady && H.resources[0].id=="native")
''',
'pool_request_filter_sent_to_server_and_retained': '''
A.refresh=fn(){A.refresh_page()}
A.owner_requests(connection)
assert(H.page=="inbox" && H.owner.poolId=="pool_original")
assert(H.calls[0].service=="hagency.pools.agents.list" && H.calls[0].args.poolId=="pool_original" && H.calls[0].args.needsMyAction)
reply(0,{items:[] nextAfter:"page_2"})
A.reqs_load(H.reqs.next)
assert(H.calls[1].args.poolId=="pool_original" && H.calls[1].args.after=="page_2")
reply(1,{items:[] nextAfter:nil})
A.inbox_filter(false)
assert(H.calls[2].args.poolId=="pool_original" && !H.calls[2].args.needsMyAction)
''',
'manager_requests_never_inherit_owner_filter': '''
// The manager also loads personal agents, independent of this pool query.
A.load_local_list=fn(){}
H.owner.poolId="owner_pool" H.entry_role="manager" H.page="pool_agents"
A.load_pool_agents(nil)
assert(H.calls[0].args.to_json()=={after:nil needsMyAction:false}.to_json())
H.pool_agents=[{id:"own" managerMxid:"@owner:test" resourceOwnerMxid:"@someone:test" poolId:"another"}]
assert(A.pool_agent_rows().len()==1)
''',
'capacity_uses_original_connection_and_native_review': '''
H.connection_capacity={id:connection.id state:"ready" pool:{publishedTokens:nil grantRevision:nil}}
A.open_connection=fn(id){H.opened=id}
A.owner_grant(connection)
assert(H.calls.len()==1 && H.calls[0].service=="hagency.onboarding.status")
reply(0,{grantReviewAvailable:true})
assert(H.calls[1].service=="hagency.grants.review" && H.calls[1].args.contributionId=="pool_original")
reply(1,{state:"cancelled"})
assert(H.opened=="pool_original" && H.calls.len()==2)
''',
'capacity_unavailable_other_owner_or_device': '''
H.connection_capacity={id:connection.id state:"ready" pool:{publishedTokens:nil grantRevision:nil}}
connection.ownerMxid="@other:test" A.owner_grant(connection) assert(H.calls.len()==0)
connection.ownerMxid="@owner:test" connection.executorDeviceId="other" A.owner_grant(connection) assert(H.calls.len()==0)
connection.executorDeviceId="device" connection.state="pending_admission" A.owner_grant(connection) assert(H.calls.len()==0)
connection.state="admitted" A.owner_grant(connection)
reply(0,{grantReviewAvailable:false})
assert(H.calls.len()==1 && H.notice!="")
''',

'published_capacity_uses_original_grant_not_new_contribution': '''
H.connection_capacity={id:connection.id state:"ready" pool:{publishedTokens:1000000 grantRevision:1 canRequest:false}}
A.open_connection=fn(id){H.opened=id}
assert(A.connection_capacity_active(connection))
A.owner_grant(connection)
reply(0,{grantReviewAvailable:true grantIntents:[{intentId:"unrelated" contributionId:"other_pool"} {intentId:"original_grant" contributionId:connection.id}]})
assert(H.calls[1].service=="hagency.grants.review" && H.calls[1].args.to_json()=={intentId:"original_grant"}.to_json())
reply(1,{state:"server_observed"})
assert(H.opened==connection.id)
''',
'published_capacity_without_original_record_cannot_create_duplicate': '''
H.connection_capacity={id:connection.id state:"ready" pool:{publishedTokens:1000000 grantRevision:1}}
A.owner_grant(connection)
reply(0,{grantReviewAvailable:true grantIntents:[{intentId:"wrong" contributionId:"another_pool"}]})
assert(H.calls.len()==1 && H.notice!="")
''',
'unknown_capacity_never_opens_publication': '''
for state in ["loading" "error" "missing" "unsupported"]{
 H.connection_capacity={id:connection.id state:state pool:nil}
 A.owner_grant(connection)
}
assert(H.calls.len()==0)
''',
'capacity_lookup_paginates_exact_pool_and_owner': '''
H.page="action" H.selected={contribution:connection}
A.load_connection_capacity(connection)
reply(0,{items:[{id:"other" ownerMxid:"@owner:test" publishedTokens:9000000 grantRevision:1}] nextAfter:"cursor"})
assert(H.calls[1].args.after=="cursor" && H.connection_capacity.state=="loading")
reply(1,{items:[{id:connection.id ownerMxid:"@owner:test" publishedTokens:1000000 grantRevision:1 canRequest:false}] nextAfter:nil})
assert(A.connection_capacity_active(connection) && H.connection_capacity.pool.publishedTokens==1000000)
''',
'capacity_lookup_failure_or_wrong_owner_is_unknown': '''
H.page="action" H.selected={contribution:connection}
A.load_connection_capacity(connection)
H.callbacks[0]({is_ok:false error:"management_unavailable"})
assert(H.connection_capacity.state=="error" && !A.connection_capacity_active(connection))
A.load_connection_capacity(connection)
reply(1,{items:[{id:connection.id ownerMxid:"@other:test" publishedTokens:1000000 grantRevision:1}] nextAfter:nil})
assert(H.connection_capacity.state=="error")
A.load_connection_capacity(connection)
reply(2,{items:[] nextAfter:"loop"})
reply(3,{items:[] nextAfter:"loop"})
assert(H.connection_capacity.state=="error" && H.calls.len()==4)
''',
'capacity_older_reply_cannot_replace_newer_state': '''
H.page="action" H.selected={contribution:connection}
A.load_connection_capacity(connection)
A.load_connection_capacity(connection)
reply(1,{items:[{id:connection.id ownerMxid:"@owner:test" publishedTokens:1000000 grantRevision:1}] nextAfter:nil})
reply(0,{items:[] nextAfter:nil})
assert(A.connection_capacity_active(connection))
A.load_connection_capacity(connection) H.selected={contribution:{id:"other"}}
reply(2,{items:[] nextAfter:nil})
assert(H.connection_capacity.state=="loading")
''',
'capacity_losing_active_grant_is_reflected_on_refresh': '''
H.page="action" H.selected={contribution:connection}
A.load_connection_capacity(connection)
reply(0,{items:[{id:connection.id ownerMxid:"@owner:test" publishedTokens:1000000 grantRevision:1}] nextAfter:nil})
assert(A.connection_capacity_active(connection))
A.load_connection_capacity(connection)
reply(1,{items:[{id:connection.id ownerMxid:"@owner:test" publishedTokens:nil grantRevision:nil}] nextAfter:nil})
assert(!A.connection_capacity_active(connection) && H.connection_capacity.state=="ready")
''',
'expired_check_does_not_invalidate_admission_or_capacity': '''
connection.verification={status:"verified" verifiedAtMs:100 freshUntilMs:300100}
H.connection_capacity={id:connection.id state:"ready" pool:{publishedTokens:1000000 grantRevision:1}}
A.observe_verification(connection,300101)
assert(connection.verification.status=="stale" && connection.state=="admitted" && A.connection_capacity_active(connection))
assert(A.connection_status(connection)==A.t("Resource pool published","资源池已发布"))
assert(H.calls.len()==0)
''',

'existing_account_creates_pool_without_redeclaring_shared_limit': '''
let model={model:"gpt-6-astra" label:"Astra" sharedReasoning:["medium","high"] sharedDefaultReasoning:"medium"}
let row={id:"account_original" revision:"account_revision" resourceCount:1}
H.account_detail={account:row resources:[{id:"source_resource" revision:"source_revision"}]}
H.account_models={accountId:row.id revision:row.revision enrollmentReady:true provider:{models:[model]}}
A.choose_account_model(row,model)
assert(H.calls[0].service=="hagency.resources.get" && H.calls[0].args.id=="source_resource")
reply(0,{protocol:"hagency-management-v1" resource:{id:"source_resource" revision:"fresh_source_revision"} choices:[{model:model.model reasoning:"medium"}] editing:{reason:nil}})
H.enrollment_draft.tokens="20000000"
A.submit=fn(service,args){H.submitted={service:service args:args}}
A.enroll_account()
assert(H.submitted.service=="hagency.resources.create")
assert(H.submitted.args.sourceResourceId=="source_resource" && H.submitted.args.expectedRevision=="fresh_source_revision")
assert(H.submitted.args.ceiling.tokens==20000000 && H.submitted.args.profile.model==model.model)
assert(H.calls.len()==1)
''',
'model_selection_is_independent_and_keeps_pool_amount_and_source': '''
let astra={model:"gpt-6-astra" label:"Astra" sharedReasoning:["medium","high"] sharedDefaultReasoning:"medium"}
let terra={model:"gpt-6.1-terra" label:"Terra" sharedReasoning:["low","high"] sharedDefaultReasoning:"low"}
let luna={model:"gpt-6-luna" label:"Luna" sharedReasoning:["medium","high"] sharedDefaultReasoning:"medium"}
H.enrollment_draft={account:{accountId:"original" expectedRevision:"revision"} model:astra models:[astra,terra,luna] reasoning:"medium" tokens:"20000000" sourceRequired:true source:{id:"source" revision:"source_revision"}}
let original=H.enrollment_draft.account.to_json() let source=H.enrollment_draft.source.to_json()
A.enrollment_model_pick("gpt-6.1-terra")
assert(H.enrollment_draft.model.model=="gpt-6.1-terra" && H.enrollment_draft.reasoning=="low")
A.enrollment_model_pick("gpt-6-luna")
assert(H.enrollment_draft.model.model=="gpt-6-luna" && H.enrollment_draft.reasoning=="medium")
assert(H.enrollment_draft.account.to_json()==original && H.enrollment_draft.source.to_json()==source && H.enrollment_draft.tokens=="20000000")
A.enrollment_model_pick("forged") assert(H.enrollment_draft.model.model=="gpt-6-luna" && H.calls.len()==0)
''',
'failed_source_read_cannot_fall_back_to_first_enrollment': '''
H.enrollment_draft={account:{accountId:"original" expectedRevision:"revision"} model:{model:"chosen" sharedReasoning:["medium"]} reasoning:"medium" tokens:"20000000" sourceRequired:true source:nil}
A.submit=fn(service,args){H.submitted={service:service args:args}}
H.submitted=nil A.enroll_account()
assert(H.submitted==nil && H.notice!="")
''',
'approval_exact_revision_and_owner_only': '''
H.pool_detail={id:"request_original" revision:17 resourceOwnerMxid:"@owner:test" managerMxid:"@manager:test" canDecide:true canCancel:false canStopSupply:false}
H.pool_confirm="approve"
A.submit=fn(service,args){H.submitted={service:service args:args}}
A.pool_decide()
assert(H.submitted.service=="hagency.pools.agents.decide" && H.submitted.args.requestId=="request_original" && H.submitted.args.expectedRevision==17 && H.submitted.args.approve)
H.submitted=nil H.entry_role="manager" A.pool_decide() assert(H.submitted==nil)
H.entry_role="owner" H.pool_detail.canDecide=false A.pool_decide() assert(H.submitted==nil)
H.pool_detail.canDecide=true H.pool_detail.resourceOwnerMxid="@other:test" A.pool_decide() assert(H.submitted==nil)
''',
'role_and_navigation_preserve_pending_work': '''
H.pool_draft={name:"Draft" tokens:"123456"} H.command={service:"hagency.accounts.login" args:{commandId:"original"}}
let draft=H.pool_draft.to_json() let command=H.command.to_json()
A.entry_choose("owner") A.entry_switch() A.entry_choose("manager")
assert(H.pool_draft.to_json()==draft && H.command.to_json()==command && H.calls.len()==0)
H.entry_role="owner" H.page="provider_account" assert(A.entry_parent()=="accounts")
H.page="resources" assert(A.entry_parent()=="resources")
H.page="pool_agent" assert(A.entry_parent()=="inbox")
''',
}
def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
 source=(APP/'bundle/main.splash').read_text();results=[]
 for lang in ('en','zh-CN'):
  for name,case in CASES.items():
   path=a.output/(lang+'-'+name+'.splash');path.write_text(fixture(source+SETUP+case,lang))
   r=subprocess.run([str(a.vm.resolve()),'root',str(path)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
   path.with_suffix('.log').write_text(r.stdout);results.append(dict(case=name,language=lang,passed=r.returncode==0))
 (a.output/'report.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps(dict(passed=sum(r['passed'] for r in results),total=len(results))));raise SystemExit(not all(r['passed'] for r in results))
if __name__=='__main__':main()
