#!/usr/bin/env python3
"""A pool whose running Agent holds the whole monthly maximum, and the list of who may request.

Reproduces what an owner hit on a published pool (9 M a month, one running Agent holding all
9 M): the per-request floor came from an estimate that missed the Agent, so the page offered
1 M - 9 M; the maximum's lock reason hid the amendment's own; nothing said the month was all
given out; and a username typed with a capital was refused while a misspelt one counted.
"""
import argparse, json, subprocess
from pathlib import Path
from check_full_bundle import APP, fixture
SETUP='''
A.redraw=fn(){} A.save=fn(){return true}
// No device storage here: the per-pool drafts start empty and stay in memory.
let fs={exists:fn(path){return false} read:fn(path){return "{}"} write:fn(path,data){return true}}
H.pool_drafts={account:"@owner:test" quota:[]}
H.ready=true H.busy=false H.command=nil H.calls=[] H.callbacks=[] H.entry_role="owner" H.page="resource_detail"
H.session={account:"@owner:test" device:"device" canAdmitResources:false features:["resource-pool-agents-v1","pool-grant-succession-v1"]}
host.request=fn(service,args,done){H.calls.push({service:service args:args}) H.callbacks.push(done)}
let reply=fn(index,data){H.callbacks[index]({is_ok:true data:data})}
let find=fn(service){let at={i:-1} for index call in H.calls {if call.service==service && at.i<0 {at.i=index}} return at.i}
let now=1793334588000
let pool=fn(id,name,ceiling,remaining,committed,spent,quota){return {id:id model:"model-"+id framework:"codex" reasoning:"medium" published:false capabilities:["coding"] localName:{name:name revision:1} budget:{remainingTokens:remaining reserved:committed pool:{ceiling:ceiling committed:committed period:"monthly"} seat:{quota:quota}} usage:{spent:spent spend_period_key:"2026-10"}}}
let main=pool("resource_main","Research",9000000,0,9000000,350000,50000000)
let spare=pool("resource_spare","Spare",20000000,20000000,0,0,50000000)
let conn={id:"con_main" label:"Research" resourceId:"resource_main" ownerMxid:"@owner:test" coordinatorMxid:"@owner:test" executorDeviceId:"device" state:"admitted"}
let spare_conn={id:"con_spare" label:"Spare" resourceId:"resource_spare" ownerMxid:"@owner:test" coordinatorMxid:"@owner:test" executorDeviceId:"device" state:"admitted"}
let grant={id:"con_main" ownerMxid:"@owner:test" publishedTokens:9000000 grantRevision:1 currentPeriodEndMs:1793491200000}
let detail=fn(row,amend){return {protocol:"hagency-management-v1" resource:row editing:{allowed:false reason:"previously_offered"} choices:[] ceiling:{tokens:row.budget.pool.ceiling period:"monthly"} ceilingAmendment:amend observedAtMs:now}}
let amendment={allowed:true reason:nil sequence:1 minimumTokens:9000000 perRequestTokens:9000000}
let open=fn(row){
    H.resources=[main,spare]
    H.resource_detail=detail(row,amendment)
    H.pool_info={rid:row.id conn:conn connState:"ready" grant:grant grantState:"ready" board:{grantReviewAvailable:true} boardState:"ready" requests:0 alerts:0 tools:0 stopped:0 workspaces:0 agents:1 inuse:1 now:now using:[] floor:nil}
    A.pool_edit_sync()
}
let floor_reply={contributionId:"con_main" heldTokens:9000000 heldAgents:1 perRequestTokens:9000000 mostTokens:9000000 periodEndMs:1793491200000}
'''
CASES={
'published_parent_is_not_counted_again_when_approving_an_agent': '''
main.budget.pool.ceiling=500000000 main.budget.pool.committed=500000000 main.budget.remainingTokens=0
open(main)
let g={id:"con_main" ownerMxid:"@owner:test" publishedTokens:500000000 perRequestTokens:125000000 grantRevision:1 currentPeriodEndMs:1793491200000}
H.pool_info.grant=g H.pool_info.floor={held:0 agents:0 most:500000000 perRequest:125000000}
H.pool_meta={conns:[conn] grants:[g] requests:[] counts:[] now:now}
let q={poolId:"con_main" requestedTokens:30000000}
assert(A.reqs_blocked(q)=="")
assert(A.reqs_after(q).search("470 M")>=0)
H.pool_raise={rid:"resource_main" drawn:0 tokens:nil name:"Cucumber" asked:30000000}
A.pool_raise_apply()
assert(H.pool_raise==nil)
assert(H.pool_edit.max==H.pool_edit.maxBase)
// The real child ledger still blocks an insufficient pool, and the Agent
// maximum is separate from the pool's total.
H.pool_info.floor.held=490000000
assert(A.reqs_blocked(q).search("10 M")>=0)
q.requestedTokens=126000000
assert(A.reqs_blocked(q).search("125 M")>=0)
// No native reading must stay unknown. The server/native reservation checks
// capacity after consent; 0 parent headroom cannot disable owner approval.
q.requestedTokens=30000000 H.pool_info=nil
assert(A.reqs_blocked(q)=="" && A.reqs_after(q)=="")
g.nativeAvailableTokens=20000000
assert(A.reqs_blocked(q).search("20 M")>=0)
g.nativeAvailableTokens=500000000
assert(A.reqs_blocked(q)=="")
''',
'recurring_terminal_and_renewal_statuses_are_truthful': '''
let row={state:"rejected" reservationState:"pending" blockingReason:nil runtimeState:"idle" currentPeriod:{state:"revoked"}}
assert(A.pool_tone(row)=="attention")
assert(A.pool_period_state(row)==A.t("Request declined","申请已拒绝"))
row.state="expired"
assert(A.pool_tone(row)=="")
assert(A.pool_period_state(row)==A.t("Request expired","申请已过期"))
row.state="open" row.currentPeriod.state="renewal_refused" row.blockingReason="agent_period_renewal_refused"
assert(A.pool_tone(row)=="attention")
assert(A.pool_period_state(row).search(A.t("Renewal refused","续期被拒绝"))>=0)
row.currentPeriod.state="request_limit_exceeded"
assert(A.pool_period_state(row).search(A.t("per-Agent maximum","单个 Agent 上限"))>=0)
for code in ["agent_generation_overflow","agent_period_terms_changed","pool_agent_request_limit_exceeded"] {
    assert(A.pool_reason(code).search(A.t("saved request is retained","已保存的请求"))<0)
}
''',
'completed_resource_retry_replaces_submitting_notice': '''
open(main)
H.resource_id="resource_main"
let args={commandId:"same-request" resourceId:"resource_main" expectedRevision:"old" expectedCeilingSequence:1 ceiling:{kind:"monthly" tokens:500000000}}
for code in ["management_ceiling_below_commitments","management_outcome_unknown","resource_edit_cancelled","provider_confirmation_expired","native_confirmation_expired"] {
    H.calls=[] H.callbacks=[] H.command={service:"hagency.resources.edit" args:args} H.pool_result=nil
    A.retry_command()
    assert(H.busy && H.calls[0].args.commandId=="same-request")
    H.callbacks[0]({is_ok:false error:code})
    assert(!H.busy && H.notice=="" && A.settings_feedback().kind=="pool")
    assert(A.settings_feedback().text.search(A.t("Submitting","正在提交"))<0)
    if code=="management_outcome_unknown" || code=="native_confirmation_expired" {assert(H.command.args.commandId=="same-request")}
    else {assert(H.command==nil)}
    A.settings_feedback_done("pool")
    assert(A.settings_feedback()==nil)
}
''',

'pool_summary_reads_published_defaults_instead_of_original_configuration': '''
open(main)
H.resource_detail.executionPolicy={yolo:false}
H.pool_info=nil
A.pool_info_load("resource_main")
let guard={n:0}
while find("hagency.web_catalog.get")<0 && guard.n<20 {
    let at=H.calls.len()-1
    let service=H.calls[at].service
    reply(at,{items:if service=="hagency.connections.list" {[conn]} else if service=="hagency.pools.list" {[grant]} else {[]} nextAfter:nil observedAtMs:now})
    guard.n=guard.n+1
}
let at=find("hagency.web_catalog.get")
assert(at>=0 && H.calls[at].args.resourceId=="resource_main")
assert(A.pool_default_yolo(H.resource_detail)==nil)
reply(at,{resourceId:"resource_main" revision:"published-2" defaultYolo:true})
assert(A.pool_default_yolo(H.resource_detail)==true)
assert(!A.resource_default_yolo(H.resource_detail))
// A confirmed switch back also updates the summary. Never borrow another pool's policy.
A.pool_default_received("resource_main",{resourceId:"resource_main" revision:"published-3" defaultYolo:false})
assert(A.pool_default_yolo(H.resource_detail)==false)
H.resource_detail.resource.id="different"
assert(A.pool_default_yolo(H.resource_detail)==nil)
H.resource_detail.editing.allowed=true H.resource_detail.executionPolicy.yolo=true
assert(A.pool_default_yolo(H.resource_detail)==true)
''',
'split_total_and_request_limit_are_preserved_on_native_handoff': '''
main.budget.pool.ceiling=20000000
open(main)
H.pool_info.grant={id:"con_main" ownerMxid:"@owner:test" publishedTokens:20000000 perRequestTokens:5000000 grantRevision:1 currentPeriodEndMs:1793491200000}
let v=A.pool_view(H.resource_detail)
assert(v.publishedTokens==5000000 && v.totalTokens==20000000)
main.budget.pool.committed=20000000 main.budget.remainingTokens=0
H.pool_info.floor=A.pool_floor_read({is_ok:true data:{contributionId:"con_main" heldTokens:5000000 heldAgents:0 mostTokens:20000000}},"con_main")
v=A.pool_view(H.resource_detail)
assert(A.pool_available(v)==15000000 && !A.pool_full(v) && v.status.key=="published")
H.pool_meta={conns:[conn] grants:[H.pool_info.grant] requests:[] counts:[] now:now}
let card=A.pool_row(main)
assert(card.status.key=="published" && !card.low && card.capacity.search("20 M")>=0 && card.capacity.search("5 M")>=0)

H.resource_id="resource_main"
A.pool_succeed("amendment",5000000,nil)
let call=H.calls[H.calls.len()-1]
assert(call.args.proposedTokens==20000000 && call.args.proposedPerRequestTokens==5000000)
assert(A.pool_limit({publishedTokens:20000000 perRequestTokens:5000000})==5000000)
assert(A.pool_limit({publishedTokens:20000000 perRequestTokens:5000000 nativeAvailableTokens:3000000})==3000000)
assert(A.pool_limit({publishedTokens:20000000 perRequestTokens:0})==nil)
assert(A.pool_limit({publishedTokens:20000000 perRequestTokens:20000001})==nil)
''',

'pool_deletion_explains_authorization_and_process_blocks_without_claiming_agents_are_running': '''
H.pool_sub_rid="resource_main" H.pool_adv_state="ready"
H.web.resourceId="resource_main" H.web.catalog={canRetire:false retireBlockedReason:"active_grant"}
assert(!A.pool_adv_can_delete() && !A.pool_adv_agents_blocked())
assert(A.pool_adv_delete_note().search(A.t("authorization","授权"))>=0)
H.web.catalog.retireBlockedReason="runtime_not_stopped"
assert(A.pool_adv_agents_blocked())
assert(A.pool_adv_delete_note().search(A.t("process","进程"))>=0)
H.web.catalog.retireBlockedReason="agent_templates_exist"
assert(!A.pool_adv_agents_blocked())
assert(A.pool_adv_delete_note().search(A.t("templates","模板"))>=0)
H.web.catalog.retireBlockedReason="resource_in_use"
assert(!A.pool_adv_agents_blocked())
H.web.catalog.canRetire=true H.web.catalog.retireBlockedReason=nil
assert(A.pool_adv_can_delete())
assert(A.pool_adv_delete_note().search(A.t("unresolved usage","未结清"))>=0)
''',
'aggregate_pool_hold_does_not_raise_per_agent_minimum': '''
open(main)
let v=A.pool_view(H.resource_detail)
assert(v.quotaMode=="amend" && v.publishedTokens==9000000)
// The running Agent isn't among the requests the page counts: the estimate knows no floor,
// which is how the page offered 1 M - 9 M.
assert(A.pool_quota_floor(v)==nil && A.pool_quota_min(v)==1 && A.pool_quota_fixed(v)==nil)
// The window's own record says what running Agents hold.
H.pool_info.floor=A.pool_floor_read({is_ok:true data:floor_reply},"con_main")
assert(A.pool_quota_floor(v)==nil && A.pool_quota_min(v)==1)
assert(H.pool_info.floor.held==9000000 && H.pool_info.floor.agents==1)
// A reply for another pool, or a refusal, is no floor.
assert(A.pool_floor_read({is_ok:true data:{contributionId:"con_other" heldTokens:1}},"con_main")==nil)
assert(A.pool_floor_read({is_ok:false error:"management_grant_succession_required"},"con_main")==nil)
''',
'the_page_reads_the_floor_of_a_published_month': '''
H.resources=[main] H.resource_detail=detail(main,amendment) H.pool_info=nil
A.pool_info_load("resource_main")
assert(H.calls[0].service=="hagency.connections.list")
reply(0,{items:[conn] nextAfter:nil observedAtMs:now})
assert(H.calls[1].service=="hagency.pools.list")
reply(1,{items:[grant] nextAfter:nil observedAtMs:now})
let guard={n:0}
while find("hagency.grants.floor")<0 && guard.n<8 {reply(H.calls.len()-1,{items:[] nextAfter:nil}) guard.n=guard.n+1}
let at=find("hagency.grants.floor")
assert(at>=0 && H.calls[at].args.contributionId=="con_main")
reply(at,floor_reply)
assert(H.pool_info.floor.held==9000000 && H.pool_info.floor.most==9000000 && H.pool_info.floor.agents==1)
''',
'per_agent_limit_can_be_lowered_even_when_total_is_all_reserved': '''
open(main)
H.pool_info.floor=A.pool_floor_read({is_ok:true data:floor_reply},"con_main")
let v=A.pool_view(H.resource_detail)
assert(A.pool_quota_fixed(v)==nil && A.pool_quota_min(v)==1)
// The resource total still cannot be lowered below aggregate commitments.
assert(v.maxMin==9000000)
H.pool_info.floor=A.pool_floor_read({is_ok:true data:{contributionId:"con_main" heldTokens:9000000 heldAgents:1 mostTokens:20000000}},"con_main")
assert(A.pool_quota_fixed(v)==nil && A.pool_quota_min(v)==1)
''',
'a_month_all_given_out_says_so_and_how_to_give_more': '''
open(main)
H.pool_meta={epoch:0 conns:[conn,spare_conn] connsError:false grants:[grant,{id:"con_spare" ownerMxid:"@owner:test" publishedTokens:nil grantRevision:nil}] grantsError:false requests:[] counts:[] now:now board:nil boardState:"ready"}
H.pool_info.floor=A.pool_floor_read({is_ok:true data:floor_reply},"con_main")
let v=A.pool_view(H.resource_detail)
assert(A.pool_full(v) && v.status.key=="full")
assert(A.pool_full_title(v)==A.t("October’s 9 M is all given out","10 月的 9 M 已全部分出"))
assert(A.pool_full_why(v).search("9 M")>=0)
// Ways forward: a higher maximum here, or Spare, admitted and not published this month.
assert(A.pool_can_raise(v) && A.pool_full_no_raise(v)=="")
let others=A.pool_full_others(v)
assert(others.len()==1 && others[0].rid=="resource_spare" && others[0].kind=="publish")
assert(A.pool_full_other_label(others[0])==A.t("Publish ","发布 ")+"Spare")
// Raising preselects room for one more request at this month's limit.
H.pool_raise={rid:"resource_main" drawn:0 tokens:nil name:nil asked:9000000 why:"full"}
A.pool_raise_apply()
assert(H.pool_raise.drawn==18000000 && A.pool_whole(H.pool_edit.max)>=18000000 && A.pool_raising("resource_main"))
assert(A.pool_raise_line("resource_main").search("18 M")>=0)
// At the account limit the maximum can't go up, and the card says so instead of offering it.
let capped=pool("resource_main","Research",9000000,0,9000000,350000,9000000)
open(capped)
H.pool_info.floor=A.pool_floor_read({is_ok:true data:floor_reply},"con_main")
let w=A.pool_view(H.resource_detail)
assert(!A.pool_can_raise(w) && A.pool_full_no_raise(w).search("9 M")>=0)
''',
'the_maximums_lock_reason_is_the_amendments_own': '''
// Given-out capacity never locks the maximum: an offered pool amends it. The amendment's reason wins.
let refused={allowed:false reason:"monthly_ceiling_required" sequence:0 minimumTokens:nil perRequestTokens:nil}
assert(A.pool_max_reason("retained_commitments",refused)==A.pool_max_reason(nil,refused))
assert(A.pool_max_reason("retained_commitments",refused).search(A.t("without a monthly maximum","没有设每月上限"))>=0)
// Without the amendment (an older Hagency) the update is the way forward.
assert(A.pool_max_reason("retained_commitments",nil)==A.pool_lock_rule())
// An allowed amendment of a pool with given-out capacity can go up.
open(main)
let v=A.pool_view(H.resource_detail)
assert(v.maxMode=="amend" && v.maxMin==9000000 && v.maxMax==50000000)
''',
'typed_names_are_lower_case_and_count_once_they_exist': '''
open(main)
H.who.rid="resource_main" H.who.picks=[]
// "LiSi" was refused for its capitals; it means @lisi.
assert(A.who_mxid("LiSi")=="@lisi:test" && A.who_mxid("@ZHAOLIU:test")=="@zhaoliu:test")
assert(A.who_mxid("li si")==nil && A.who_mxid("@lisi:elsewhere.test")==nil)
// A typed name counts only once the server says it exists: the misspelt "zhaolu" never did.
H.who.name="zhaolu"
A.who_search(H.who.name)
assert(A.who_typed()==nil && A.who_count()==0)
timers[timers.len()-1]()
let at=find("hagency.people.search")
assert(at>=0 && H.calls[at].args.query=="zhaolu")
reply(at,{server:"test" query:"zhaolu" people:[] exact:{userId:"@zhaolu:test" exists:false}})
assert(A.who_typed()==nil && A.who_count()==0 && A.who_name_bad()!="")
H.who.name="LiSi"
A.who_search(H.who.name)
timers[timers.len()-1]()
let at2=H.calls.len()-1
assert(H.calls[at2].service=="hagency.people.search" && H.calls[at2].args.query=="lisi")
reply(at2,{server:"test" query:"lisi" people:[{userId:"@lisi:test" displayName:"Li Si"}] exact:{userId:"@lisi:test" exists:true}})
assert(A.who_typed()=="@lisi:test" && A.who_count()==1 && A.who_name_bad()=="")
let hits=A.who_hits()
assert(hits.len()==1 && hits[0].id=="@lisi:test" && hits[0].name=="Li Si")
// A later keystroke drops an earlier answer.
H.who.name="li" A.who_search(H.who.name)
let late=H.calls.len()-1
assert(A.who_typed()==nil)
''',
'the_starting_list_is_the_people_the_owner_chats_with': '''
open(main)
H.who.rid="resource_main" H.who.picks=[] H.who.name=""
H.who_list={rid:"resource_main" cid:"con_main" state:"ready" data:{contributionId:"con_main" state:"published" applicants:["@wangwu:test"] changePending:false} code:""}
H.who_people.rid="" H.who_people.knownState="idle"
A.who_people_load("resource_main")
let at=find("hagency.people.search")
assert(at>=0 && H.calls[at].args.query=="")
reply(at,{server:"test" query:"" people:[{userId:"@lisi:test" displayName:"Li Si"},{userId:"@wangwu:test" displayName:"Wang Wu"},{userId:"@zhaoliu:test" displayName:"Zhao Liu"}] exact:nil})
let v=A.pool_view(H.resource_detail)
let people=A.who_candidates(v)
// Everyone the owner chats with on this server, except who is already on this month's list.
assert(people.len()==2 && people[0]=="@lisi:test" && people[1]=="@zhaoliu:test")
assert(A.who_display("@zhaoliu:test")=="Zhao Liu" && A.who_source(v,"@zhaoliu:test")==A.t("In a chat with you","和你在同一个聊天里"))
''',
# The owner closed a pool's server connection; the last grant the pool list knows still names
# what was published. A closed connection publishes nothing, on every page that says so.
'a_closed_connection_is_not_published': '''
open(main)
H.connection_capacity={id:"con_main" epoch:1 state:"ready" pool:grant}
assert(A.connection_capacity_active(conn))
assert(A.sub_connection_status(conn)==A.t("Published · ready for Agents","已发布，可以使用"))
let closed={id:"con_main" label:"Research" resourceId:"resource_main" ownerMxid:"@owner:test" coordinatorMxid:"@owner:test" executorDeviceId:"device" state:"revoked"}
assert(!A.connection_capacity_active(closed))
assert(A.sub_connection_status(closed)!=A.t("Published · ready for Agents","已发布，可以使用"))
assert(A.server_status(closed).text==A.t("Connection closed","连接已关闭"))
assert(!A.pool_published(closed,grant))
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
