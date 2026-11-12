#!/usr/bin/env python3
"""A sync of a personal Agent's settings that the device closed, from the shipped bundle.

"Sync settings" only reads the computer that runs the Agent. When that computer
answers "failed" or does not answer, the request is over: the UI drops it, says why,
and lets the person sync again. A save with the same answer keeps its request,
because a save may have been applied. No HTTP or provider effects.
"""
import argparse,json,subprocess
from pathlib import Path
from check_full_bundle import fixture,APP
SETUP='''
let H=mod.hagency let A=mod.control_actions
A.redraw=fn(){return nil} H.busy=false H.ready=true H.notice="" H.command=nil
H.session={account:"@owner:test" features:[]}
let calls=[] let callbacks=[] let acknowledgements=[]
host.request=fn(service,args,done){calls.push({service:service args:args}) callbacks.push(done)}
A.save=fn(){return true} A.acknowledge=fn(id){acknowledgements.push(id)}
H.personal_id="agent_a" H.personal_mode="detail" H.page="personal_agent"
let observe={service:"hagency.personal_agents.observe" args:{commandId:"original_observation" agentId:"agent_a" expectedGeneration:3}}
let save={service:"hagency.personal_agents.policy.save" args:{commandId:"original_save" agentId:"agent_a" expectedGeneration:3 expectedRevision:4}}
'''
CLOSED=['configuration_provider_unavailable','configuration_local_state_unavailable','configuration_request_failed','configuration_operation_unsupported','configuration_model_unavailable','invalid_arguments','configuration_runtime_busy','configuration_authority_changed','configuration_authority_invalidated','configuration_target_changed','configuration_request_expired','configuration_outcome_unknown']
CASES={}
for code in CLOSED:
 CASES['sync_closed_by_'+code]='''
H.command=observe H.personal_draft={agentId:"agent_a"}
A.retry_command()
assert(calls.len()==1 && calls[0].service=="hagency.personal_agents.observe" && calls[0].args.commandId=="original_observation")
callbacks[0]({is_ok:false error:"%s"})
assert(H.command==nil)
assert(acknowledgements.len()==1 && acknowledgements[0]=="original_observation")
assert(H.local_failure=="hagency.personal_agents.observe" && H.local_failure_code=="%s")
assert(H.local_failure_reason!="" && H.local_failure_reason==A.observe_reason("%s"))
assert(H.local_kept==nil && H.personal_draft==nil && H.personal_mode=="detail" && H.page=="personal_agent")
let reread=false for call in calls{if call.service=="hagency.personal_agents.get" && call.args.id=="agent_a"{reread=true}}
assert(reread)
'''%(code,code,code)
 CASES['save_with_'+code+'_keeps_its_request']='''
H.command=save H.personal_draft={tokens:"250"}
A.retry_command()
callbacks[0]({is_ok:false error:"%s"})
assert(H.command!=nil && H.command.args.commandId=="original_save")
assert(acknowledgements.len()==0 && H.local_failure=="")
'''%code
CASES['sync_with_an_unknown_answer_keeps_its_request']='''
H.command=observe H.personal_draft={agentId:"agent_a"}
A.retry_command()
callbacks[0]({is_ok:false error:"management_outcome_unknown"})
assert(H.command!=nil && H.command.args.commandId=="original_observation")
assert(acknowledgements.len()==0 && H.local_failure=="")
A.retry_command()
assert(calls.len()==2 && calls[1].args.commandId=="original_observation")
'''
CASES['each_closed_answer_has_its_own_words']='''
let busy=A.observe_reason("configuration_runtime_busy") let late=A.observe_reason("configuration_request_expired")
let moved=A.observe_reason("configuration_target_changed") let other=A.observe_reason("configuration_provider_unavailable")
assert(busy!=late && busy!=moved && busy!=other && late!=moved && late!=other && moved!=other)
assert(late==A.observe_reason("configuration_outcome_unknown"))
assert(moved==A.observe_reason("configuration_authority_changed") && moved==A.observe_reason("configuration_authority_invalidated"))
assert(!A.observe_closed("management_outcome_unknown") && !A.observe_closed("configuration_unsupported") && !A.observe_closed(""))
assert(!A.personal_not_applied("hagency.personal_agents.policy.save","configuration_provider_unavailable"))
assert(A.personal_not_applied("hagency.personal_agents.observe","configuration_provider_unavailable"))
'''
def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--bundle',type=Path,default=APP/'bundle/main.splash');p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True);source=a.bundle.read_text()
 results=[]
 for lang in ['en','zh-CN']:
  for name,test in CASES.items():
   path=a.output/f'{lang}-{name}.splash';path.write_text(fixture(source+'\nmod.control_actions=A\n',lang)+SETUP+test+'\ntrue\n')
   r=subprocess.run([str(a.vm.resolve()),'eval',str(path)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True);path.with_suffix('.log').write_text(r.stdout);results.append({'language':lang,'case':name,'passed':r.returncode==0})
 (a.output/'report.json').write_text(json.dumps({'checks':results},indent=2)+'\n');print(json.dumps({'passed':sum(r['passed'] for r in results),'total':len(results)}));raise SystemExit(not all(r['passed'] for r in results))
if __name__=='__main__':main()
