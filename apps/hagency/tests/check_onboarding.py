#!/usr/bin/env python3
"""Evaluate current setup views and native handoffs in the pinned VM."""
import argparse
import json
from pathlib import Path
import subprocess
from check_agent_admission import PREFIX, APP

SETUP = '''
mod.rinx_icons.connection=5
mod.widgets.RINX_SPACING=1
H.page="onboarding" H.session.account="@owner:test" H.device_from=""
H.onboarding=nil H.onboarding_connections=[] H.onboarding_after=nil H.onboarding_result=nil
H.resources=[{id:"resource-one" model:"Current resource"}]
A.resource_title=fn(row){return row.model}
A.resource_short_id=fn(id){return id}
A.observe_verification=fn(row,observed){H.observed=observed}
A.navigate=fn(page){H.page=page}
A.open_accounts=fn(after){H.page="provider_accounts"}
A.pool_device_open=fn(from){H.page="pool_device"}
let native={state:"uninitialized" canReview:true grantReviewAvailable:false grantIntents:[]}
let original={intentId:"original-intent" contributionId:"own" resourceId:"resource-one" state:"saved"}
'''

CASES = {
 'loading': ('H.busy=true', 'assert(button("onboarding_review")==nil && !button("onboarding_refresh").enabled)'),
 'failed_read': ('', 'assert(has(A.t("Refresh to read the current native requirements.","请刷新以读取当前原生条件。")))'),
 'current_setup_handoff_and_close': ('H.onboarding=native', '''
assert(button("onboarding_review").text==A.t("Set up this device","设置此设备"))
button("onboarding_review").on_click()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.onboarding.review" && H.calls[0].args.to_json()=={}.to_json())
H.callback({state:"review_closed"})
assert(H.calls.len()==2 && H.calls[1].service=="hagency.onboarding.status")
assert(H.onboarding==nil && H.onboarding_result.state=="review_closed")
'''),
 'setup_does_not_create_accounts_or_quota': ('H.onboarding=native', '''
assert(has(A.t("Set up local management for your signed-in Matrix account. No provider account or shared quota is created in this step.","为当前登录的 Matrix 账号设置本机管理。此步骤不会创建模型服务账号或共享额度。")))
assert(button("onboarding_accounts")==nil && H.calls.len()==0)
'''),
 'active_opens_current_accounts': ('H.onboarding=native native.state="active" native.canReview=false', '''
assert(button("onboarding_review")==nil)
button("onboarding_accounts").on_click() assert(H.page=="provider_accounts" && H.calls.len()==0)
'''),
 'active_cannot_repeat_setup': ('H.onboarding=native native.state="active"', '''
assert(button("onboarding_review")==nil) A.review_onboarding() assert(H.calls.len()==0)
'''),
 'unavailable_never_reviews': ('H.onboarding=native native.state="unavailable" native.canReview=false native.reason="onboarding_unavailable"', '''
assert(button("onboarding_review")==nil && has(A.onboarding_reason("onboarding_unavailable")))
A.review_onboarding() assert(H.calls.len()==0)
'''),
 'current_owner_blocker': ('H.onboarding=native native.canReview=false native.reason="management_scope_changed"', '''
assert(button("onboarding_review")==nil && has(A.onboarding_reason("management_scope_changed")))
'''),
 'inventory_does_not_grant_publication_authority': ('H.onboarding=native native.state="active" native.canReview=false', '''
assert(button("onboarding_grant_recover")==nil)
A.review_grant({intentId:"forged"}) assert(H.calls.len()==0)
'''),
 'original_publication_handoff': ('H.onboarding=native native.state="active" native.canReview=false native.grantReviewAvailable=true native.grantIntents=[original]', '''
assert(has("Current resource")) button("onboarding_grant_recover").on_click()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.grants.review" && H.calls[0].args.to_json()=={intentId:"original-intent"}.to_json())
H.callback({state:"server_observed"})
assert(H.calls.len()==2 && H.calls[1].service=="hagency.onboarding.status" && H.onboarding_result.state=="server_observed")
'''),
 'connections_remain_separate': ('H.onboarding=native native.state="active" native.canReview=false', '''
button("onboarding_connections").on_click() assert(H.page=="connections" && H.calls.len()==0)
'''),
 'uncertain_current_setup': ('H.onboarding=native H.onboarding_result={state:"outcome_unknown"}', '''
assert(has(A.t("Result unknown · recover original","结果未知 · 请恢复原始操作")))
assert(button("onboarding_review")!=nil && H.calls.len()==0)
'''),
 'closed_grant_keeps_reserved_amounts': ('H.onboarding=native native.state="active" native.canReview=false H.onboarding_result={state:"recorded" closure:{retainedTokens:200 knownConsumedTokens:40 unresolvedTokens:160}}', '''
assert(has(A.t("Previous grant closed. Retained amounts remain committed; a new grant requires a separately approved admission.","旧的发布已关闭；Agent 已拿到的额度仍算在本月已用和已分出里。新的发布需要重新通过加入审批。")))
'''),
 'local_reservation_is_not_server_publication': ('H.onboarding=native native.state="active" native.canReview=false H.onboarding_result={state:"recorded"}', '''
assert(has(A.t("Capacity is reserved. Open the saved publication to continue.","这台电脑已记下这次发布，打开发布记录就能继续。")))
'''),
 'busy_blocks_native_actions': ('H.onboarding=native H.busy=true', '''
assert(!button("onboarding_review").enabled)
A.review_onboarding() A.open_onboarding() assert(H.calls.len()==0)
'''),
 'current_status_rejects_old_or_inconsistent_protocol': ('', '''
for state in ["legacy","recovery_required","untrusted"] {
 A.onboarding_received({state:state canReview:true}) assert(H.onboarding==nil && H.notice!="")
}
A.onboarding_received({state:"active" canReview:true}) assert(H.onboarding==nil)
A.onboarding_received({state:"uninitialized" canReview:true grantReviewAvailable:true}) assert(H.onboarding==nil)
A.onboarding_received({state:"active" canReview:false})
assert(H.onboarding.state=="active" && !H.onboarding.grantReviewAvailable && H.onboarding.grantIntents.len()==0)
'''),
}

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('vm',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    args.output.mkdir(parents=True,exist_ok=True)
    source=(APP/'bundle/main.splash').read_text()
    helpers=source[source.index('A.onboarding_reason = fn('):source.index('A.readmission_enabled = fn(')]
    start=source.index('        if H.page == "onboarding" {')
    page=source[start:source.index('        if H.page == "provider_accounts" ||',start)]
    results=[]
    for language in ('en','zh-CN'):
        for name,(setup,checks) in CASES.items():
            script=PREFIX+helpers+SETUP
            if language=='zh-CN':script+='A.t=fn(en,zh){return zh}\n'
            script+=setup+'\nlet root=View{'+page+'}\ncollect(root)\n'+checks
            script+='''
for id in ["tokens","provider","grant_confirm","publish","execute"] {assert(button(id)==nil)}
H.busy=false H.calls=[] H.command={commandId:"keep-original"}
button("onboarding_refresh").on_click()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.onboarding.status" && H.calls[0].args.to_json()=={}.to_json() && H.onboarding==nil)
H.callback({state:"active" canReview:false})
assert(H.command.commandId=="keep-original")
button("onboarding_back").on_click() assert(H.page=="provider_accounts")
true
'''
            path=args.output/f'{language}-{name}.splash'
            path.write_text(script)
            run=subprocess.run([str(args.vm.resolve()),'eval',str(path)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            path.with_suffix('.log').write_text(run.stdout)
            results.append({'case':name,'language':language,'passed':run.returncode==0})
    (args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps({'passed':sum(row['passed'] for row in results),'total':len(results)}))
    raise SystemExit(not all(row['passed'] for row in results))

if __name__=='__main__':main()
