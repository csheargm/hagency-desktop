#!/usr/bin/env python3
"""Native controls on production pages with explicit offline state fixtures.

Only initial state/connection is substituted. Rendering and input callbacks are
the shipped bundle. This is UI acceptance, not backend workflow execution.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from native_workspaces import Workspaces
from native_pool_application import ROOT

SESSION=dict(protocol="hagency-management-v1",account="@reviewer:example.test",administrator="@admin:example.test",device="offline",deviceGeneration=1,canAdmitResources=True,features=["resource-pool-agents-v1"])
MODEL=dict(id="qualified",model="qualified",label="Coding and documentation",sharedReasoning=["low","medium","high"],sharedDefaultReasoning="medium")
BUDGET=dict(requestId="request",requestRevision=3,assignmentGeneration=2,assignmentId="assignment",agentReservationId="reservation",originalCeilingTokens=1000000,appliedDeltaTokens=0,effectiveCeilingTokens=1000000,budgetRevision=7,budgetDigest="a"*64,knownConsumedTokens=420000,conservativeCommitmentTokens=500000,nativeRetainedTokens=None,nativeRemainingTokens=580000,usageComplete=True,executionSupported=True,canRequest=True,canOwnerFund=True,canOwnerFundDirect=True,blockingReason=None)
CASES={
    "enrollment":dict(page="account_enrollment",entry_role="owner",enrollment_draft=dict(account=dict(accountId="account",expectedRevision="revision"),model=MODEL,reasoning="medium",tokens="1000000")),
    "resource":dict(page="resource_edit",entry_role="owner",resource_edit_draft=dict(resourceId="resource",expectedRevision="revision",yolo=False,model="qualified",reasoning="medium",tokens="1000000",limitMode="monthly"),resource_detail=dict(resource=dict(id="resource",revision="revision",model="qualified",framework="codex",localName=dict(name="Codex resource",revision=1)),choices=[dict(model="qualified",reasoning=e) for e in ["low","medium","high"]],executionPolicy=dict(yolo=False))),
    "budget":dict(page="agent_budget",entry_role="owner",budget_request_id="request",budget_agent_name="小白",agent_budget=dict(supported=True,budget=BUDGET,observedAtUtc="2026-10-28 12:00 UTC"),budget_topups=dict(topUps=dict(items=[],nextAfter=None))),
    "model":dict(page="personal_model",entry_role="owner",personal_id="agent",personal_config=dict(epoch=1,offset=0,result=None,choices=dict(agentId="agent",generation=1,profileDigest="digest",models=[dict(runtime="codex",model="qualified",displayName="Coding and research",reasoningEfforts=["low","medium","high"])],workspaceConfigured=True,canChooseWorkspace=True),draft=dict(agentId="agent",expectedGeneration=1,expectedProfileDigest="digest",runtime="codex",model="qualified",reasoningEffort="medium",webSearch=False,workspace="preserve"))),
    "task":dict(page="operator_task_edit",entry_role="manager",task_phase="editing",task_draft=dict(operation="create",title="Create task",input=dict(title="",description="",priority="p2",granularity="task",assignee="",parentId="",labels=[]))),
    "wizard":dict(page="wizard",entry_role="owner",wizard=dict(step=2,runtime="codex",model=MODEL,reasoning="medium",tokens="1000000",label="",coordinator="",resource=None,admission=None),providers=[dict(runtime="codex",connected=True,accountVerified=True,models=[MODEL])]),
}

class Family(Workspaces):
    def __init__(self,out,binary,language,narrow,case):
        self.out=out;self.trace=[];self.profile=out/"profile";(self.profile/"sandbox").mkdir(parents=True)
        (self.profile/"ui-language.json").write_text(json.dumps(language));self.state_path=self.profile/"sandbox/onboarding-v1.json"
        source=(ROOT/"apps/hagency/bundle/main.splash").read_text()
        state=dict(ready=True,busy=False,session=SESSION,**CASES[case])
        bootstrap='A.connect=fn(){\nlet fixture='+json.dumps(json.dumps(state,ensure_ascii=False),ensure_ascii=False)+'.parse_json()\n'+''.join('H.'+key+'=fixture.'+key+'\n' for key in state)+'A.redraw()\n}\n'
        marker='if !mod.rinx_theme.reloading() {start_timeout'
        assert source.count(marker)==1
        self.source_path=out/"fixture-source.splash";self.source_path.write_text(source.replace(marker,bootstrap+marker))
        (out/"initial-state.json").write_text(json.dumps(state,ensure_ascii=False,indent=2))
        self.fixture_path=out/"fixtures.json";self.fixture_path.write_text('{}')
        with socket.socket() as sock:sock.bind(("127.0.0.1",0));port=sock.getsockname()[1]
        self.base=f"http://127.0.0.1:{port}";self.log=(out/"native.log").open('w')
        self.process=subprocess.Popen([str(binary),f"--remote={port}"]+(["--narrow"] if narrow else []),cwd=ROOT,stdout=self.log,stderr=subprocess.STDOUT,env=dict(os.environ,HAGENCY_DESKTOP_DATA_DIR=str(self.profile),HAGENCY_UX_FIXTURES=str(self.fixture_path),HAGENCY_UX_SOURCE=str(self.source_path),MAKEPAD_HIDE_WINDOWS="1",MAKEPAD_NO_FOCUS="1"))
        self.wait(lambda:self.req('/s')['pid']==self.process.pid)
        self.wait(lambda:self.row('entry_switch')['r'][2]>0)

    def field(self,name,value,separate=False):
        self.click(name);self.req('/k',c='A',cmd=1,wait=1)
        if separate:
            self.req('/k',c='Backspace',wait=1)
            for char in value:self.req('/t',t=char,wait=1)
        else:self.req('/t',t=value,wait=1)

    def number(self,name):
        self.field('input','0.123456',True)
        self.wait(lambda:self.row(name).get('t')=='123456')
        slider=self.visible('amount_slider');self.click_row(slider)
        self.req('/k',c='End',wait=1)
        self.wait(lambda:self.row(name).get('t')=='10000000')
        self.req('/k',c='ArrowLeft',wait=1)
        self.wait(lambda:self.row(name).get('t')=='9900000')
        assert not any(r.get('i') in {'increase','decrease'} and r['r'][2]>0 and r['r'][3]>0 for r in self.snap())
        self.field('input','0.223456',True)
        self.req('/event',data='ux:dark');time.sleep(.3)
        assert self.row(name).get('t')=='223456'
        self.visible(name);self.shot('02-dark-number')

    def run(self,case):
        self.shot('01-initial')
        if case in {'enrollment','resource','wizard'}:
            name={'enrollment':'enrollment_tokens','resource':'resource_config_tokens','wizard':'budget'}[case]
            if case!='resource':self.number(name)
            if case=='enrollment':
                self.field('input','0');self.click('enrollment_review')
                self.wait(lambda:any('greater than zero' in r.get('t','') or '大于零的整数' in r.get('t','') for r in self.snap()))
                self.field('input','0.345678',True)
                self.wait(lambda:self.row(name).get('t')=='345678')
            if case=='resource':
                self.click('resource_config_unrestricted');self.click('resource_config_standard')
                # The monthly maximum is edited on the pool page; this editor keeps the model and permissions.
                assert not any(r.get('i')==name for r in self.snap())
        elif case=='budget':
            self.click('budget_request');self.number('budget_tokens')
            self.click('budget_review');self.wait(lambda:self.row('budget_submit')['enabled'])
            assert any('0.223456 M' in r.get('t','') for r in self.snap())
            self.shot('03-owner-single-confirmation');self.click('budget_edit')
            assert self.row('budget_tokens').get('t')=='223456'
        elif case=='model':
            efforts=[r for r in self.snap() if r.get('i')=='personal_model_effort']
            assert len(efforts)==3, "Each reported reasoning level needs its own selectable control"
            before=self.row('personal_model_search')['t'];self.click('personal_model_search')
            self.wait(lambda:self.row('personal_model_search')['t']!=before)
            self.click('personal_workspace_default');self.req('/event',data='ux:dark');time.sleep(.3)
            assert self.row('personal_model_search')['t']!=before
            self.shot('02-toggle-dark');self.click('personal_model_search')
            self.wait(lambda:self.row('personal_model_search')['t']==before)
        elif case=='task':
            self.field('task_title_input','Research 中文 123',True)
            self.click('task_options_toggle');self.field('task_assignee_input','小白')
            self.click('task_options_toggle');self.req('/event',data='ux:dark');time.sleep(.3)
            assert self.row('task_title_input')['t']=='Research 中文 123'
            self.click('task_options_toggle');assert self.row('task_assignee_input')['t']=='小白'
            self.shot('02-disclosure-dark')
        self.req('/event',data='ux:violet');time.sleep(.3);self.shot('04-violet-retained')
        if case == 'enrollment':
            self.req('/event',data='ux:large-text');time.sleep(.3)
            self.wait(lambda:self.row('input')['r'][3]>=55)
            assert self.row('enrollment_tokens')['t']=='345678'
            # The header is one line; the account shows on the role chooser instead.
            assert not any(r.get('i')=='workspace_account' and r['r'][3]>0 for r in self.snap())
            self.visible('enrollment_tokens');self.shot('05-large-text-retained')

def main():
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True);p.add_argument('--binary',type=Path,default=ROOT/'target/release/examples/hagency_visual');p.add_argument('--case',choices=CASES);a=p.parse_args()
    a.output.mkdir(parents=True,exist_ok=True);results=[]
    for lang,narrow in [('en',False),('zh-CN',True)]:
        for case in [a.case] if a.case else CASES:
            out=a.output.resolve()/(case+'-'+lang);app=Family(out,a.binary.resolve(),lang,narrow,case)
            try:app.run(case)
            except Exception:app.shot('failure');raise
            finally:app.stop()
            results.append(dict(case=case,language=lang,narrow=narrow,passed=True))
            (a.output/'report.json').write_text(json.dumps(dict(checks=results,source_sha256=hashlib.sha256((ROOT/'apps/hagency/bundle/main.splash').read_bytes()).hexdigest(),binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest()),indent=2)+'\n')
            print(results[-1],flush=True)

if __name__=='__main__':main()
