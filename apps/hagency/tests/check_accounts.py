#!/usr/bin/env python3
"""Exercise shipped account views and asynchronous routing in the pinned VM."""
import argparse,json,subprocess
from pathlib import Path
from check_agent_admission import APP,PREFIX

def main():
    p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
    source=(APP/'bundle/main.splash').read_text()
    recovery=source[source.index('A.recover_native = fn('):source.index('A.saved_label = fn(')]
    helpers=source[source.index('A.begin_setup = fn('):source.index('A.fields_changed = fn(')]
    start=source.index('        if H.page == "provider_accounts" {View{')
    pages=source[start:source.index('        if A.sub_page() &&',start)]
    fixture='''
let Down=3
mod.widgets.RINX_FIELD=1
mod.widgets.RINX_INFO_BG=1 mod.widgets.RINX_INFO_FG=1
mod.widgets.HagencyList=base mod.widgets.HagencyFitRow=base mod.widgets.HagencyRowBody=base mod.widgets.HagencyRowIcon=base
mod.widgets.HagencyRowGlyph=base mod.widgets.HagencyListText=base mod.widgets.HagencyWrapTitle=base mod.widgets.HagencyWrapSubtitle=base
mod.widgets.HagencyChevron=base mod.widgets.HagencyRowCover=base
let Inset={} let Align={}
mod.widgets.HagencyColumn=base mod.widgets.HagencyLargeTitle=base mod.widgets.HagencyListNote=base mod.widgets.HagencyListLabel=base
mod.widgets.HagencyListRow=base mod.widgets.HagencyListBody=base mod.widgets.HagencyListValue=base mod.widgets.HagencyListDivider=base
mod.widgets.HagencyListTap=base mod.widgets.HagencyListTitle=base mod.widgets.HagencyAccentText=base mod.widgets.RINX_MUTED=1
mod.widgets.RINX_SPACING=1 mod.rinx_icons.connection=1 mod.rinx_icons.close=1
H.ux.accountId="" H.ux.resourceJourney=false
H.owner={accountId:"" accountLabel:"" accountTab:"login" creatingPool:false setupOpen:false}
A.resource_title=fn(r){return r.model}
A.tokens=fn(v){return ""+v+" M"}
mod.rinx_icons.person=1
mod.rinx_icons.device=1
H.account_list_epoch=0 H.account_pending_open=true
H.enrollment_models_open=false
H.account_models=nil H.account_models_attempted=false H.account_models_epoch=0 H.enrollment_draft=nil
H.page="provider_accounts" H.account_inventory=nil H.account_detail=nil H.account_id="account_original" H.account_after=nil H.account_resource_after=nil
H.wizard={step:0 resource:nil admission:nil}
H.entry_role=""
A.navigate=fn(page){H.page=page}
A.reasoning_label=fn(effort){return effort}
A.refresh=fn(){if H.page=="provider_accounts"{A.open_accounts(H.account_after)}else{A.open_account(H.account_id,H.account_resource_after)}}
A.open_resource=fn(id){H.page="resource_detail" H.resource_id=id}
let row={id:"account_original" identityKey:"identity_one" kind:"managed_namespace" runtime:"codex" state:"active" ordinal:1 revision:"revision" profile:"codex-default-namespace-v1" readiness:"unknown" resourceCount:1}
let inventory={protocol:"hagency-management-v1" observedAtMs:1000 accounts:[row] account:nil resources:[] nextAfter:nil}
let detail={protocol:"hagency-management-v1" observedAtMs:1000 accounts:[] account:row resources:[{id:"resource_original" model:"Coding model"}] nextAfter:nil}
let second={id:"account_second" identityKey:"identity_one" kind:"managed_namespace" runtime:"codex" state:"active" ordinal:2 revision:"revision" profile:"codex-default-namespace-v1" readiness:"subscription" resourceCount:0}
let claude={id:"account_claude_code" kind:"local_configuration" runtime:"claude" state:"active" revision:"revision_claude" readiness:"subscription" resourceCount:1}
H.pool_create=nil H.pool_create_seq=0
'''
    cases={
      'fresh_setup_entry':('', 'A.begin_setup()\nassert(H.page=="provider_accounts" && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.list")'),
      'new_resource_entry':('', 'A.new_resource()\nassert(H.page=="provider_accounts" && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.list")'),
      'removed_provider_wizard_entry':('H.wizard.step=2', 'A.begin_setup()\nassert(H.page=="provider_accounts" && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.list")'),
      'retained_setup_entry':('H.wizard.step=3 H.wizard.resource={resourceId:"original"}', 'A.begin_setup()\nassert(H.page=="wizard" && H.wizard.resource.resourceId=="original" && H.calls.len()==0)'),
      'pending_setup_entry':('H.command={commandId:"original"}', 'A.begin_setup()\nassert(H.page=="provider_accounts" && H.command.commandId=="original" && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.list")'),
      'retirement_entry':('H.page="provider_account" H.account_detail=detail row.resourceCount=0','button("account_retire").on_click()\nassert(H.calls.len()==1 && H.calls[0].service=="hagency.accounts.retire" && H.calls[0].args.accountId==row.id)'),
      'retirement_bound':('H.page="provider_account" H.account_detail=detail row.resourceCount=1','assert(button("account_retire")==nil)'),
      'retirement_retired':('H.page="provider_account" H.account_detail=detail row.state="retired"','assert(button("account_retire")==nil)\nassert(has(A.t("Hagency no longer uses this account space. Provider sign-out has not been verified; credentials are retained.","Hagency 已停用此账号空间。模型服务商的退出登录尚未验证；凭据仍保留。")))'),
      'retirement_pending':('H.page="provider_account" H.account_detail=detail row.resourceCount=0 H.command={service:"original"}','assert(!button("account_retire").enabled)\nbutton("account_retire").on_click()\nassert(H.calls.len()==0)'),
      'enrollment_context':('H.page="account_enrollment" H.enrollment_draft={account:{accountId:row.id expectedRevision:row.revision} model:{label:"Coding" model:"gpt-5.6-sol" sharedReasoning:["medium","high"]} reasoning:"medium" tokens:"1000"}', 'assert(has("Coding") && has("account_original"))\nassert(has(A.t("Declared monthly token limit","声明的每月 token 上限")))\nbutton("enrollment_review").on_click()\nassert(H.calls.len()==1 && H.calls[0].service=="hagency.accounts.enroll" && H.calls[0].args.monthlyTokens==1000)'),
      'enrollment_back':('H.page="account_enrollment" H.enrollment_draft={account:{accountId:row.id expectedRevision:row.revision} model:{label:"Coding" model:"gpt-5.6-sol" sharedReasoning:["medium"]} reasoning:"medium" tokens:"1000"}', 'button("enrollment_back").on_click()\nassert(H.page=="provider_account" && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.get")'),
      'enrollment_pending':('H.page="account_enrollment" H.command={service:"original"} H.enrollment_draft={account:{accountId:row.id expectedRevision:row.revision} model:{label:"Coding" model:"gpt-5.6-sol" sharedReasoning:["medium"]} reasoning:"medium" tokens:"1000"}', 'assert(!button("enrollment_review").enabled)\nbutton("enrollment_review").on_click()\nassert(H.calls.len()==0)'),
      'prepare_review':('H.page="account_prepare"','assert(has(A.t("Add your Codex account", "添加你的 Codex 账号")))\nassert(has(A.t("Next: sign in on the provider’s page", "下一步：在服务商页面登录")))\nbutton("account_prepare_confirm").on_click()\nassert(H.calls.len()==1 && H.calls[0].service=="hagency.accounts.prepare")'),
      'prepare_cancel':('H.page="account_prepare"','button("account_prepare_cancel").on_click()\nassert(H.page=="provider_accounts" && H.calls[0].service=="hagency.accounts.list")'),
      'prepare_busy':('H.page="account_prepare" H.busy=true','assert(!button("account_prepare_confirm").enabled)\nbutton("account_prepare_confirm").on_click()\nassert(H.calls.len()==0)'),
      'prepare_pending':('H.page="account_prepare" H.command={service:"hagency.accounts.prepare"}','assert(!button("account_prepare_confirm").enabled)\nbutton("account_prepare_confirm").on_click()\nassert(H.calls.len()==0)'),
      'prepare_entry':('H.account_inventory=inventory','button("account_prepare_review").on_click()\nassert(H.page=="provider_accounts" && H.calls.len()==1 && H.calls[0].service=="hagency.onboarding.status")\nH.callback({state:"active"})\nassert(H.page=="account_prepare")'),
      'prepare_needs_setup':('H.account_inventory=inventory A.dev_open=fn(page,from){H.page=page}','button("account_prepare_review").on_click()\nH.callback({state:"uninitialized"})\nassert(H.page=="pool_device_setup" && H.calls.len()==1)'),
      'prepare_late_setup':('H.account_inventory=inventory','button("account_prepare_review").on_click()\nH.page="overview"\nH.callback({state:"active"})\nassert(H.page=="overview")'),
      'loading':('H.busy=true','assert(has(A.t("Reading accounts…","正在读取账号…")))\nassert(button("account_prepare_review")==nil && !button("owner_setup").enabled)'),
      'failed':('','assert(has(A.t("Account list unavailable","账号列表暂不可用")))\nassert(!has(A.t("No Codex account yet","还没有 Codex 账号")))'),
      'empty':('H.account_inventory=inventory inventory.accounts=[]','assert(has(A.t("No Codex account yet","还没有 Codex 账号")) && has(A.t("Not connected yet","尚未接入")))\nassert(button("account_prepare_review").enabled && button("account_open")==nil && button("account_local_pool")==nil && button("account_pending_toggle")==nil)'),
      # One group per coding tool, each ending in its own action; nothing is left outside a group.
      'tools_grouped':('H.account_inventory=inventory inventory.accounts=[row,claude] A.dev_open=fn(page,from){H.page=page}','assert(has("CODEX") && has("CLAUDE CODE") && has(A.t("DEVICE","设备")) && !has(A.t("OTHER RECORDS","其他记录")))\nassert(has(A.t("Connect a Codex Account","接入 Codex 账号")) && has(A.t("New Pool","新建资源池")))\nassert(!has(A.t("Not connected yet","尚未接入")) && !has(A.t("No Codex account yet","还没有 Codex 账号")))\nbutton("owner_setup").on_click()\nassert(H.page=="pool_device")'),
      'shared_identity':('H.account_inventory=inventory inventory.accounts=[row,second]','assert(has(A.t("Codex account ","Codex 账号 ")+"1 · "+A.account_kind(row)+" 1") && has(A.t("Codex account ","Codex 账号 ")+"1 · "+A.account_kind(row)+" 2"))\nassert(has(A.t("These login records share a recorded account identity. Actions apply to the selected login only.","这些登录记录关联同一账号身份。操作仅作用于所选登录记录。")))\nassert(has(A.t("Subscription login observed","已检测到订阅登录")))'),
      'unconfirmed_closed':('H.account_inventory=inventory row.identityKey=nil H.account_pending_open=false','assert(has(A.t("Unconfirmed records","待确认记录")) && has("1") && button("account_record_open")==nil)\nbutton("account_pending_toggle").on_click()\nassert(H.account_pending_open)'),
      'managed':('H.account_inventory=inventory','assert(has(A.t("Codex account ","Codex 账号 ")+"1"))\nassert(has(A.t("Login status not verified · pools: 1","登录状态未验证 · 已有 1 个资源池")))\nbutton("account_open").on_click()\nassert(H.calls[0].service=="hagency.accounts.get" && H.calls[0].args.id=="account_original")'),
      # Claude Code as this computer has it set up is a coding tool of its own on this page, not a record to confirm.
      'claude_code_listed':('H.account_inventory=inventory inventory.accounts=[row,claude]','let g=A.account_groups()\nassert(g.local.len()==1 && g.unconfirmed.len()==0 && g.known.len()==1)\nassert(has("Claude Code"))\nassert(has(A.pool_login_text(claude)))\nassert(button("account_local_pool").enabled)'),
      'claude_code_new_pool':('H.account_inventory=inventory inventory.accounts=[row,claude]','button("account_local_pool").on_click()\nassert(H.page=="pool_create" && H.pool_create.account==nil && H.calls.len()==1 && H.calls[0].service=="hagency.accounts.list")\nH.host_callback({is_ok:true data:{accounts:[row,claude]}})\nassert(H.pool_create.account.id=="account_claude_code" && H.pool_create.accounts.len()==2)\nassert(H.calls.len()==2 && H.calls[1].service=="hagency.accounts.get" && H.calls[1].args.id=="account_claude_code")'),
      'claude_code_pending':('H.account_inventory=inventory inventory.accounts=[row,claude] H.command={service:"original"}','assert(!button("account_local_pool").enabled)\nbutton("account_local_pool").on_click()\nassert(H.page=="provider_accounts" && H.calls.len()==0)'),
      'uncertain':('H.account_inventory=inventory row.identityKey=nil row.state="uncertain"','assert(has(A.t("Preparation outcome unknown","准备结果未确认")+" · "+A.account_readiness(row)+A.t(" · pools: 1"," · 已有 1 个资源池")))'),
      'retired':('H.account_inventory=inventory row.identityKey=nil row.state="retired"','assert(has(A.t("Retired","已停用")+" · "+A.account_readiness(row)+A.t(" · pools: 1"," · 已有 1 个资源池")))'),
      'linked':('H.account_inventory=inventory row.identityKey=nil row.kind="resource_binding" row.state="linked"','assert(has(A.t("Recorded resource association","已记录资源关联")+" · "+A.account_readiness(row)+A.t(" · pools: 1"," · 已有 1 个资源池")))'),
      'observed':('H.account_inventory=inventory row.readiness="subscription" row.resourceCount=0','assert(has(A.t("Subscription login observed","已检测到订阅登录")))'),
      'list_pagination':('H.account_inventory=inventory inventory.nextAfter="account_original"','A.open_accounts(nil)\nH.callback(inventory)\nassert(H.account_inventory==nil && H.calls.len()==2 && H.calls[1].service=="hagency.accounts.list" && H.calls[1].args.after=="account_original")'),
      'models_entry':('H.page="provider_account" H.account_detail=detail','button("account_models_check").on_click()\nassert(H.calls.len()==1 && H.calls[0].service=="hagency.accounts.models" && H.calls[0].args.accountId=="account_original")\nassert(H.busy && H.account_models==nil)'),
      'models_empty':('H.page="provider_account" H.account_detail=detail H.account_models_attempted=true H.account_models={provider:{models:[]}}','assert(has(A.t("This account returned no available models.","此账号未返回可用模型。")))'),
      'models_need_setup':('H.page="provider_account" H.account_detail=detail H.account_models_attempted=true H.account_models={enrollmentReady:false provider:{models:[{label:"Original model" model:"original-model" sharedReasoning:["high"]}]}} A.dev_open=fn(page,from){H.page=page}', 'assert(!button("account_model_enroll").visible)\nassert(has(A.t("Finish device setup before enrolling resources from this account.","请先完成设备设置，再从此账号登记资源。")))\nbutton("account_device_setup").on_click()\nassert(H.page=="pool_device_setup")'),
      'models_can_enroll':('H.page="provider_account" H.account_detail=detail H.account_models_attempted=true H.account_models={accountId:row.id revision:row.revision enrollmentReady:true provider:{models:[{label:"Original model" model:"original-model" sharedReasoning:["high"] sharedDefaultReasoning:"high"}]}}', 'assert(button("account_model_enroll").visible)\nbutton("account_model_enroll").on_click()\nassert(H.page=="account_enrollment" && H.enrollment_draft.account.accountId==row.id && H.enrollment_draft.model.model=="original-model" && H.enrollment_draft.reasoning=="high")'),
      'models_visible':('H.page="provider_account" H.account_detail=detail H.account_models_attempted=true H.account_models={provider:{models:[{label:"Original model" model:"original-model" sharedReasoning:["high"]}]}}','assert(has("Original model") && has("original-model"))\nassert(has(A.t("Eligible model choice; resource setup is still required","可选模型；仍需完成资源接入")))'),
      'detail_retired':('H.page="provider_account" H.account_detail=detail row.state="retired"','assert(button("account_models_check")==nil)\nassert(has(A.t("Retired","已停用")))'),
      'detail_linked':('H.page="provider_account" H.account_detail=detail row.kind="resource_binding" row.state="linked"','assert(button("account_models_check")==nil)\nassert(has(A.t("Recorded resource association","已记录资源关联")))'),
      'detail':('H.page="provider_account" H.account_detail=detail','assert(has(A.t("Resource pools on this tool","此工具的资源池")))\nassert(has("Coding model"))\nbutton("account_resource").on_click()\nassert(H.page=="resource_detail" && H.resource_id=="resource_original")'),
      'detail_pagination':('H.page="provider_account" H.account_detail=detail detail.nextAfter="resource_original"','button("account_resources_next").on_click()\nassert(H.calls[0].service=="hagency.accounts.get" && H.calls[0].args.after=="resource_original" && H.calls[0].args.id=="account_original")'),
      'refresh':('H.page="provider_account" H.account_detail=detail','button("accounts_refresh").on_click()\nassert(H.calls.len()==1 && H.calls[0].service=="hagency.accounts.get" && H.account_detail==nil)'),
    }
    results=[]
    def run(name,code):
        path=a.output/(name+'.splash');path.write_text(code+'\ntrue\n');r=subprocess.run([str(a.vm.resolve()),'eval',str(path)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT);(a.output/(name+'.log')).write_text(r.stdout);results.append({'case':name,'passed':r.returncode==0})
    for lang in ['en','zh']:
        base=PREFIX+helpers+fixture+('A.t=fn(en,zh){return zh}\n' if lang=='zh' else '')
        for name,(state,checks) in cases.items():
            tab='H.owner.accountTab="pools"\n' if name.startswith('models_') or name in {'detail','detail_pagination'} else ''
            run(name+'-'+lang,base+state+'\n'+tab+'let root=View{'+pages+'}\ncollect(root)\n'+checks)
        for name,checks in {
          'late_list':'A.open_accounts(nil)\nH.page="overview"\nH.callback(inventory)\nassert(H.account_inventory==nil)',
          'late_detail':'A.open_account("account_original",nil)\nH.account_id="other"\nH.callback(detail)\nassert(H.account_detail==nil)',
          'wrong_account':'A.open_account("account_original",nil)\ndetail.account.id="other"\nH.callback(detail)\nassert(H.account_detail==nil && H.notice!="")',
          'busy_guard':'H.busy=true\nA.open_accounts(nil)\nA.open_account("account_original",nil)\nassert(H.calls.len()==0)',
          'same_result':'A.open_account("account_original",nil)\nH.callback(detail)\nassert(H.account_detail.account.id=="account_original")',
        }.items():run(name+'-'+lang,base+checks)
        run('saved_enrollment_recovery-'+lang,base+recovery+'''
A.personal_config_operation=fn(operation){return false}
A.is_template_operation=fn(operation){return false}
A.is_graph_operation=fn(operation){return false}
A.is_task_operation=fn(operation){return false}
H.command={commandId:"original-command"}
H.enrollment_draft={model:"draft"}
A.recover_native({commandId:"original-command" operation:"enroll_account_resource" input:{}})
assert(H.calls.len()==1 && H.calls[0].service=="hagency.intent.retry" && H.calls[0].args.commandId=="original-command")
H.callback({operation:"enroll_account_resource" data:{resourceId:"confirmed-resource"}})
assert(H.command==nil && H.enrollment_draft==nil)
assert(H.page=="resource_detail" && H.resource_id=="confirmed-resource")
assert(H.ack=="original-command")
''')
    (a.output/'report.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps({'passed':sum(r['passed'] for r in results),'total':len(results)}));raise SystemExit(not all(r['passed'] for r in results))

if __name__=='__main__':main()
