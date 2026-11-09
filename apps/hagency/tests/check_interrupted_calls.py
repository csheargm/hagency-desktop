#!/usr/bin/env python3
"""Owner reconciliation of interrupted pool-Agent calls: listing, bounds, submission."""
import argparse, json, subprocess
from pathlib import Path
from check_full_bundle import APP, fixture
SETUP='''
A.redraw=fn(){} A.save=fn(){return true}
H.ready=true H.busy=false H.command=nil H.calls=[] H.callbacks=[] H.entry_role="owner"
H.session={account:"@owner:test" device:"device" features:["resource-pool-agents-v1"]}
host.request=fn(service,args,done){H.calls.push({service:service args:args}) H.callbacks.push(done)}
let reply=fn(index,data){H.callbacks[index]({is_ok:true data:data})}
let detail={id:"par_1" agentId:"agt_1" resourceOwnerMxid:"@owner:test" managerMxid:"@edward:test" requestedTokens:50000000 definition:{name:"小黑娃" capability:"architect" description:"码工"} state:"approved_waiting_executor"}
let stuck={callId:"mac_1" reservationId:"mar_1" executionId:"sx_1" state:"issued" reservedTokens:50000000 knownConsumedTokens:0 usageObserved:false periodKey:"2026-11" live:false}
let partial={callId:"mac_2" reservationId:"mar_1" executionId:"sx_2" state:"unknown" reservedTokens:100 knownConsumedTokens:40 usageObserved:true periodKey:"2026-11" live:false}
let running={callId:"mac_3" reservationId:"mar_1" executionId:"sx_3" state:"issued" reservedTokens:100 knownConsumedTokens:0 usageObserved:false periodKey:nil live:true}
'''
CASES={
'owner_detail_loads_unsettled_calls_only_for_the_resource_owner': '''
H.page="pool_agent" H.pool_detail=detail
A.pool_calls_load(detail)
assert(H.calls.len()==1 && H.calls[0].service=="hagency.agents.calls.list" && H.calls[0].args.agentId=="agt_1")
reply(0,{agentId:"agt_1" fenced:false calls:[stuck] ledgerRevision:7})
assert(H.pool_calls!=nil && H.pool_calls.calls.len()==1 && H.pool_calls.calls[0].callId=="mac_1")
H.pool_calls=nil H.entry_role="manager" A.pool_calls_load(detail) assert(H.calls.len()==1 && H.pool_calls==nil)
H.entry_role="owner" let other={..detail resourceOwnerMxid:"@someone:test"} A.pool_calls_load(other) assert(H.calls.len()==1 && H.pool_calls==nil)
''',
'stale_list_replies_are_ignored_after_navigation': '''
H.page="pool_agent" H.pool_detail=detail
A.pool_calls_load(detail)
H.pool_detail={..detail agentId:"agt_other"}
reply(0,{agentId:"agt_1" fenced:false calls:[stuck] ledgerRevision:7})
assert(H.pool_calls==nil)
''',
'missing_usage_and_old_estimates_never_become_automatic_declarations': '''
H.page="pool_agent" H.pool_detail=detail
A.pool_call_open(stuck,"zero") assert(H.pool_confirm=="" && H.pool_call==nil)
A.pool_call_open(partial,"full") assert(H.pool_confirm=="" && H.pool_call==nil)
A.pool_call_open(stuck,"exact") assert(A.pool_call_error()!="")
H.pool_call_note="provider verified zero requests" assert(A.pool_call_error()=="")
A.submit=fn(service,args){H.submitted={service:service args:args}} A.pool_call_submit()
assert(H.submitted.args.consumedTokens==0 && H.submitted.args.expectedReservedTokens==50000000)
assert(H.submitted.args.evidenceReference=="provider verified zero requests")
''',
'verified_amount_allows_overrun_but_never_undercounts_observed_usage': '''
H.page="pool_agent" H.pool_detail=detail
A.submit=fn(service,args){H.submitted={service:service args:args}}
A.pool_call_open(partial,"exact")
assert(H.pool_call_amount=="40" && A.pool_call_error()!="")
H.pool_call_note="provider usage page"
assert(A.pool_call_error()=="")
H.pool_call_amount="39" assert(A.pool_call_error()!="")
H.pool_call_amount="101" assert(A.pool_call_error()=="")
H.pool_call_amount="9007199254740992" assert(A.pool_call_error()!="")
H.pool_call_amount="" assert(A.pool_call_error()!="")
H.pool_call_amount="160" assert(A.pool_call_error()=="")
A.pool_call_submit()
assert(H.submitted.args.consumedTokens==160 && H.submitted.args.evidenceReference=="provider usage page" && H.submitted.args.expectedReservedTokens==100)
''',
'running_calls_cannot_be_opened_and_results_refresh_the_page': '''
H.page="pool_agent" H.pool_detail=detail
A.pool_call_open(running,"zero") assert(H.pool_confirm=="" && H.pool_call==nil)
assert(A.pool_call_state(running)==A.t("Still running on this computer. Settle it after it ends.","仍在这台电脑上进行，结束后再处理。"))
H.pool_call=stuck H.pool_confirm="reconcile"
A.pool_call_received({callId:"mac_1" reservationId:"mar_1" state:"settled" consumedTokens:0 releasedTokens:50000000 observationId:"obs" ledgerRevision:8 replayed:false})
assert(H.pool_call==nil && H.pool_confirm=="" && H.calls.len()==2 && H.calls[0].service=="hagency.agents.calls.list" && H.calls[1].service=="hagency.pools.agents.get")
assert(H.notice!="")
assert(A.pool_call_result_valid({callId:"mac_1" state:"settled"},{callId:"mac_1"}) && !A.pool_call_result_valid({callId:"mac_9" state:"settled"},{callId:"mac_1"}))
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
