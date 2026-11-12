#!/usr/bin/env python3
"""Exercise the shipped recipient picker/review with the real pinned Splash VM.

Host fixtures describe closed native responses; widgets are inert. These tests
prove callbacks, identity preservation and recovery, not pixels or delivery.
"""
import argparse
import json
import subprocess
from pathlib import Path
from check_full_bundle import APP, fixture
from check_graph_workflows import SETUP

RECIPIENT_SETUP = r'''
let target={kind:"shared_agent" projectId:"project-original" agentRequestId:"request-original"}
let snapshot={graphId:"graph-original" nodeId:"second" revision:7 target:target observationDigest:"observed-original" identityJson:"{\"agentId\":\"agent-original\",\"assignmentGeneration\":7}" account:"@manager:example.test" server:"https://example.test" recipientMxid:"@agent-original:example.test" roomId:"!room-original:example.test" runtimeDeliveryAuthorized:false}
let current={graphId:"graph-original" nodeId:"second" revision:7 state:"unbound" binding:nil runtimeDeliveryAuthorized:false}
let project_next={id:"graph-original" nodeId:"second" kind:"shared_agent" projectId:"project-original"}
let project_row={id:"project-original" label:"Research project" detail:"Manager @manager:example.test" next:project_next target:nil}
let leaf={id:"request-original" label:"Writer" detail:"Agent request request-original" next:nil target:target}
let options={graphId:"graph-original" nodeId:"second" revision:7 kind:"shared_agent" stage:"projects" items:[project_row] nextAfter:nil runtimeDeliveryAuthorized:false}
let observation={graphId:"graph-original" nodeId:"second" revision:7 target:target observationDigest:"observed-original" canBind:true blockingReasons:[] account:snapshot.account server:snapshot.server recipientMxid:snapshot.recipientMxid roomId:snapshot.roomId identityJson:snapshot.identityJson runtimeDeliveryAuthorized:false}
let change={operation:"bind_recipient" nodeId:"second" snapshot:snapshot}
let binding_prepared={commandId:"graphcmd_binding_original" graphId:"graph-original" expectedRevision:7 operation:"graph_bind_recipient" change:change changeJson:change.to_json() result:nil error:nil deliveryState:"pending_binding" runtimeDeliveryAuthorized:false}
let binding_result={commandId:"graphcmd_binding_original" graphId:"graph-original" revision:8 deliveryState:"binding_saved" runtimeDeliveryAuthorized:false}
let enter=fn(){H.flow_id="graph-original" H.flow_node_id="second" H.flow_node=node H.page="operator_graph_node" A.recipient_begin() replies[0]({is_ok:true data:current})}
let choices=fn(){enter() A.recipient_choose("shared_agent") replies[1]({is_ok:true data:options})}
let select_leaf=fn(){choices() options.items=[leaf] A.recipient_pick(leaf) replies[2]({is_ok:true data:observation})}
'''

CASES = {
    'node_button_opens_exact_binding': '''H.flow_id="graph-original" H.flow_node_id="second" H.flow_node=node H.page="operator_graph_node" RENDER find("flow_recipient_set").on_click() assert(calls.len()==1 && calls[0].service=="hagency.graphs.recipient.get" && calls[0].args.to_json()=={id:"graph-original" nodeId:"second"}.to_json()) assert(H.flow_recipient.assignee=="writer" && H.flow_recipient.revision==7 && H.flow_phase=="editing")''',
    'current_get_is_read_only': '''enter() assert(calls.len()==1 && H.flow_recipient.current==current && H.flow_pending==nil) RENDER assert(has(A.recipient_state("unbound")) && has(A.recipient_notice()) && count("recipient_kind")==3)''',
    'saved_binding_displays_exact_old_identity': '''current.state="binding_saved" current.binding=snapshot enter() H.flow_recipient.currentDetails=true RENDER assert(has(A.recipient_state("binding_saved")) && has(snapshot.recipientMxid) && find("recipient_current_json").text==snapshot.identityJson)''',
    'stale_binding_keeps_saved_identity_visible': '''current.state="binding_stale" current.binding=snapshot enter() H.flow_recipient.currentDetails=true RENDER assert(has(A.recipient_state("binding_stale")) && find("recipient_current_json").text==snapshot.identityJson && count("recipient_kind")==3)''',
    'get_wrong_node_refused': '''current.nodeId="other" enter() assert(H.flow_recipient.current==nil && H.notice!="")''',
    'get_delivery_authority_refused': '''current.runtimeDeliveryAuthorized=true enter() assert(H.flow_recipient.current==nil)''',
    'get_unknown_binding_state_refused': '''current.state="running" enter() assert(H.flow_recipient.current==nil)''',
    'get_saved_without_binding_refused': '''current.state="binding_saved" enter() assert(H.flow_recipient.current==nil)''',
    'graph_revision_change_requires_node_refresh': '''current.revision=8 enter() A.recipient_choose("shared_agent") assert(H.flow_recipient.stale && calls.len()==1) RENDER assert(find("recipient_reload_node")!=nil && count("recipient_kind")==0) find("recipient_reload_node").on_click() assert(calls[1].service=="hagency.graphs.node" && calls[1].args.nodeId=="second")''',
    'kind_choice_uses_native_options_service': '''enter() RENDER let kinds=[] for row in widgets{if row.name=="recipient_kind"{kinds.push(row.value)}} kinds[0].on_click() assert(calls[1].service=="hagency.graphs.recipient.options" && calls[1].args.to_json()=={id:"graph-original" nodeId:"second" kind:"shared_agent"}.to_json())''',
    'hierarchical_project_uses_returned_query': '''choices() RENDER find("recipient_option").on_click() assert(calls[2].service=="hagency.graphs.recipient.options" && calls[2].args.to_json()==project_next.to_json() && H.flow_recipient.history.len()==1)''',
    'native_leaf_identity_not_recorded_label': '''choices() options.items=[leaf] RENDER find("recipient_option").on_click() assert(calls[2].service=="hagency.graphs.recipient.observe" && calls[2].args.target.to_json()==target.to_json() && H.flow_recipient.assignee=="writer" && H.flow_node.assignee=="writer")''',
    'duplicate_display_names_keep_exact_target': '''choices() let other={id:"request-other" label:"Writer" detail:"Second identity" next:nil target:{kind:"shared_agent" projectId:"project-original" agentRequestId:"request-other"}} options.items=[leaf,other] RENDER let buttons=[] for row in widgets{if row.name=="recipient_option"{buttons.push(row.value)}} buttons[1].on_click() assert(calls[2].args.target.agentRequestId=="request-other" && H.flow_node.assignee=="writer")''',
    'personal_hierarchy_preserves_exact_binding': '''enter() A.recipient_choose("personal_agent") options.kind="personal_agent" options.items=[{id:"personal-original" label:"My agent" detail:"Personal agent" next:{id:"graph-original" nodeId:"second" kind:"personal_agent" agentId:"personal-original"} target:nil}] replies[1]({is_ok:true data:options}) A.recipient_pick(options.items[0]) assert(calls[2].args.agentId=="personal-original") options.items=[{id:"binding-original" label:"Owner direct room" detail:"Existing room" next:nil target:{kind:"personal_agent" agentId:"personal-original" bindingId:"binding-original"}}] replies[2]({is_ok:true data:options}) A.recipient_pick(options.items[0]) assert(calls[3].args.target.bindingId=="binding-original" && calls[3].args.target.agentId=="personal-original")''',
    'human_room_member_is_exact_mxid': '''enter() A.recipient_choose("human") options.kind="human" options.items=[{id:"project-original" label:"Project" detail:"Verified project" next:{id:"graph-original" nodeId:"second" kind:"human" projectId:"project-original"} target:nil}] replies[1]({is_ok:true data:options}) A.recipient_pick(options.items[0]) options.items=[{id:"!human-room:example.test" label:"Team" detail:"Project room" next:{id:"graph-original" nodeId:"second" kind:"human" projectId:"project-original" roomId:"!human-room:example.test"} target:nil}] replies[2]({is_ok:true data:options}) A.recipient_pick(options.items[0]) assert(calls[3].args.roomId=="!human-room:example.test") options.items=[{id:"@human:example.test" label:"Writer" detail:"@human:example.test" next:nil target:{kind:"human" projectId:"project-original" roomId:"!human-room:example.test" userId:"@human:example.test"}}] replies[3]({is_ok:true data:options}) A.recipient_pick(options.items[0]) assert(calls[4].args.target.userId=="@human:example.test" && calls[4].args.target.roomId=="!human-room:example.test")''',
    'empty_page_keeps_native_cursor': '''options.items=[] options.nextAfter="next-original" choices() RENDER assert(has(A.t("No selectable identities on this page.","本页没有可选身份。"))) find("recipient_next").on_click() assert(calls[2].args.after=="next-original" && calls[2].args.kind=="shared_agent")''',
    'options_render_at_most_sixteen': '''options.items=[] for i in 0..16{options.items.push({id:"project-"+i label:"Project" detail:"Exact identity" next:project_next target:nil})} options.nextAfter="later" choices() RENDER assert(count("recipient_option")==16 && find("recipient_next")!=nil)''',
    'seventeenth_native_option_refused': '''options.items=[] for i in 0..17{options.items.push(project_row)} choices() assert(H.flow_recipient.options==nil && H.notice!="")''',
    'options_wrong_kind_refused': '''options.kind="human" choices() assert(H.flow_recipient.options==nil)''',
    'options_ambiguous_row_refused': '''project_row.target=target choices() assert(H.flow_recipient.options==nil)''',
    'options_revision_change_is_stale': '''options.revision=8 choices() assert(H.flow_recipient.options==nil && H.flow_recipient.stale)''',
    'native_wrong_graph_next_query_refused': '''choices() project_row.next.id="other" A.recipient_pick(project_row) assert(calls.len()==2 && H.notice!="")''',
    'parent_restores_exact_original_query': '''choices() A.recipient_pick(project_row) options.stage="requests" options.items=[leaf] replies[2]({is_ok:true data:options}) A.recipient_parent() assert(calls[3].args.to_json()=={id:"graph-original" nodeId:"second" kind:"shared_agent"}.to_json() && H.flow_recipient.history.len()==0)''',
    'return_to_kinds_does_not_prepare': '''choices() A.recipient_parent() assert(calls[2].service=="hagency.graphs.recipient.get" && H.flow_recipient.query==nil && H.flow_pending==nil)''',
    'busy_blocks_picker_and_prepare': '''enter() H.busy=true A.recipient_choose("shared_agent") A.recipient_observe(target) A.recipient_prepare() A.recipient_begin() assert(calls.len()==1)''',
    'stale_options_page_reply_ignored': '''enter() A.recipient_choose("shared_agent") H.page="overview" replies[1]({is_ok:true data:options}) assert(H.flow_recipient.options==nil && H.page=="overview" && !H.busy)''',
    'stale_options_epoch_does_not_clear_new_busy': '''enter() A.recipient_choose("shared_agent") H.flow_epoch=H.flow_epoch+1 H.busy=true replies[1]({is_ok:true data:options}) assert(H.busy && H.flow_recipient.options==nil)''',
    'stale_options_query_ignored': '''enter() A.recipient_choose("shared_agent") H.flow_recipient.query={id:"graph-original" nodeId:"second" kind:"human"} replies[1]({is_ok:true data:options}) assert(H.flow_recipient.options==nil)''',
    'observe_is_read_only_until_review': '''select_leaf() assert(calls.len()==3 && H.flow_recipient.observation==observation && H.flow_pending==nil && H.flow_node.assignee=="writer") H.flow_recipient.observationDetails=true RENDER assert(find("recipient_prepare")!=nil && find("recipient_identity_json").text==snapshot.identityJson && has(A.recipient_notice()))''',
    'observe_wrong_selected_target_refused': '''choices() options.items=[leaf] A.recipient_pick(leaf) observation.target={kind:"shared_agent" projectId:"project-original" agentRequestId:"different"} replies[2]({is_ok:true data:observation}) assert(H.flow_recipient.observation==nil)''',
    'observe_claimed_authority_refused': '''observation.runtimeDeliveryAuthorized=true select_leaf() assert(H.flow_recipient.observation==nil)''',
    'observe_after_target_change_ignored': '''choices() options.items=[leaf] A.recipient_pick(leaf) H.flow_recipient.target={kind:"shared_agent" projectId:"project-original" agentRequestId:"other"} replies[2]({is_ok:true data:observation}) assert(H.flow_recipient.observation==nil)''',
    'observe_graph_revision_change_refused': '''observation.revision=9 select_leaf() assert(H.flow_recipient.observation==nil && H.flow_recipient.stale)''',
    'admin_visibility_does_not_grant_sender_authority': '''observation.canBind=false observation.blockingReasons=["project_manager_required"] select_leaf() A.recipient_prepare() assert(calls.len()==3) RENDER assert(find("recipient_prepare")==nil && has(A.recipient_reason("project_manager_required")))''',
    'blocking_reason_wins_over_can_bind': '''observation.blockingReasons=["project_manager_required"] select_leaf() A.recipient_prepare() assert(calls.len()==3) RENDER assert(find("recipient_prepare")==nil)''',
    'no_observation_cannot_prepare': '''choices() A.recipient_prepare() assert(calls.len()==2 && H.flow_pending==nil)''',
    'prepare_uses_exact_observation_and_revision': '''select_leaf() RENDER find("recipient_prepare").on_click() assert(calls.len()==4 && calls[3].service=="hagency.graphs.bind_recipient" && calls[3].args.to_json()=={id:"graph-original" nodeId:"second" expectedRevision:7 target:target observationDigest:"observed-original"}.to_json()) replies[3]({is_ok:true data:binding_prepared}) assert(H.flow_phase=="saved" && H.flow_pending.commandId=="graphcmd_binding_original" && calls.len()==4)''',
    'prepare_mismatched_snapshot_is_unknown': '''select_leaf() A.recipient_prepare() binding_prepared.change.snapshot.observationDigest="different" replies[3]({is_ok:true data:binding_prepared}) assert(H.flow_phase=="unknown" && H.flow_pending==nil && calls.len()==4)''',
    'prepare_definite_identity_change_requires_reobserve': '''select_leaf() A.recipient_prepare() replies[3]({is_ok:false error:"management_recipient_changed"}) assert(H.flow_phase=="editing" && H.page=="operator_graph_recipient" && H.flow_recipient.observation==nil) A.recipient_prepare() assert(calls.len()==4) RENDER find("recipient_observe_retry").on_click() assert(calls[4].service=="hagency.graphs.recipient.observe" && calls[4].args.target.to_json()==target.to_json())''',
    'prepare_definite_unavailable_keeps_exact_selection': '''select_leaf() A.recipient_prepare() replies[3]({is_ok:false error:"management_recipient_unavailable"}) assert(H.page=="operator_graph_recipient" && H.flow_recipient.target.to_json()==target.to_json() && H.flow_recipient.observation==nil)''',
    'prepare_revision_conflict_requires_reload': '''select_leaf() A.recipient_prepare() replies[3]({is_ok:false error:"management_revision_conflict"}) assert(H.flow_recipient.stale && H.flow_phase=="editing") A.recipient_prepare() assert(calls.len()==4)''',
    'lost_prepare_never_prepares_replacement': '''select_leaf() A.recipient_prepare() replies[3]({is_ok:false error:"management_outcome_unknown"}) A.recipient_prepare() A.recipient_begin() A.flow_new() assert(calls.len()==4 && H.flow_phase=="unknown" && H.flow_pending==nil)''',
    'late_prepare_does_not_switch_pages': '''select_leaf() A.recipient_prepare() H.page="overview" replies[3]({is_ok:true data:binding_prepared}) assert(H.page=="overview" && H.flow_pending==nil)''',
    'saved_binding_restart_reviews_original_only': '''A.recover_native({operation:"graph_bind_recipient" commandId:"graphcmd_binding_original"}) assert(calls[0].service=="hagency.graphs.command.get" && calls[0].args.commandId=="graphcmd_binding_original") replies[0]({is_ok:true data:binding_prepared}) assert(H.flow_pending==binding_prepared && H.flow_phase=="saved" && calls.len()==1)''',
    'saved_binding_review_uses_authoritative_json': r'''binding_prepared.changeJson="{\"exact\":9007199254740993}" A.flow_prepared(binding_prepared) H.flow_review_json=true RENDER assert(find("recipient_saved_json").text==binding_prepared.changeJson && has(A.t("Recipient Matrix ID","接收方 Matrix ID")+" · "+snapshot.recipientMxid) && find("flow_command_apply").text==A.t("Apply saved binding","应用已保存的绑定"))''',
    'saved_binding_missing_identity_refused': '''binding_prepared.change.snapshot=nil A.flow_prepared(binding_prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'apply_only_original_command_id': '''A.flow_prepared(binding_prepared) RENDER find("flow_command_apply").on_click() assert(calls.len()==1 && calls[0].service=="hagency.graphs.command.apply" && calls[0].args.to_json()=={commandId:"graphcmd_binding_original"}.to_json()) replies[0]({is_ok:true data:binding_result}) assert(H.flow_phase=="recorded" && H.flow_pending.result.deliveryState=="binding_saved") A.flow_apply() assert(calls.len()==1)''',
    'unknown_apply_preserves_original_binding_and_checks_only': '''A.flow_prepared(binding_prepared) A.flow_apply() replies[0]({is_ok:false error:"management_outcome_unknown"}) H.flow_review_json=true RENDER assert(find("flow_command_apply")==nil && find("recipient_saved_json").text==binding_prepared.changeJson) find("flow_command_check").on_click() assert(calls[1].service=="hagency.graphs.command.get" && calls[1].args.commandId=="graphcmd_binding_original") replies[1]({is_ok:true data:binding_prepared}) assert(calls.len()==2 && H.flow_phase=="saved")''',
    'apply_wrong_graph_receipt_is_unknown': '''A.flow_prepared(binding_prepared) A.flow_apply() binding_result.graphId="other" replies[0]({is_ok:true data:binding_result}) assert(H.flow_phase=="unknown" && H.flow_pending.result==nil)''',
    'recorded_binding_receipt_does_not_claim_delivery': '''binding_prepared.result=binding_result binding_prepared.deliveryState="binding_saved" A.flow_prepared(binding_prepared) RENDER assert(find("flow_command_apply")==nil && has(A.flow_delivery("binding_saved")) && has(A.t("Recipient saved. This task has not been sent.","接收方已保存，此任务尚未发送。")))''',
    'pending_saved_binding_blocks_new_edits': '''H.saved_commands=[{operation:"graph_bind_recipient" commandId:"original" hasReply:false}] H.flow_node=node A.recipient_begin() A.flow_new() assert(H.flow_recipient==nil && H.flow_draft==nil && calls.len()==0)''',
    'failed_saved_binding_remains_reviewable': '''binding_prepared.error="management_recipient_changed" A.flow_prepared(binding_prepared) H.flow_review_json=true RENDER assert(H.flow_phase=="failed" && find("recipient_saved_json").text==binding_prepared.changeJson && find("flow_command_apply")==nil && find("flow_command_close")!=nil)''',
}


CASES.update({
    'unknown_binding_never_claims_unapplied': '''A.flow_prepared(binding_prepared) H.flow_phase="unknown" RENDER assert(has(A.t("Binding result not confirmed","绑定结果尚未确认")) && !has(A.t("Binding request saved; awaiting confirmation","绑定请求已保存，等待确认")))''',
    'refused_binding_never_awaits_confirmation': '''binding_prepared.error="management_recipient_changed" A.flow_prepared(binding_prepared) RENDER assert(has(A.t("Binding request refused","绑定请求已拒绝")) && !has(A.t("Binding request saved; awaiting confirmation","绑定请求已保存，等待确认")))''',
    'saved_snapshot_wrong_graph_refused': '''binding_prepared.change.snapshot.graphId="other" A.flow_prepared(binding_prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'saved_snapshot_wrong_node_refused': '''binding_prepared.change.snapshot.nodeId="first" A.flow_prepared(binding_prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'saved_snapshot_wrong_revision_refused': '''binding_prepared.change.snapshot.revision=8 A.flow_prepared(binding_prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'saved_snapshot_runtime_authority_refused': '''binding_prepared.change.snapshot.runtimeDeliveryAuthorized=true A.flow_prepared(binding_prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'get_nested_snapshot_wrong_node_refused': '''current.state="binding_saved" current.binding=snapshot snapshot.nodeId="other" enter() assert(H.flow_recipient.current==nil)''',
    'native_next_cannot_switch_recipient_kind': '''choices() project_row.next.kind="human" A.recipient_pick(project_row) assert(calls.len()==2 && H.flow_recipient.kind=="shared_agent" && H.notice!="")''',
    'target_only_native_row_is_selectable': '''options.items=[{id:"request-original" label:"Writer" detail:"request-original" target:target}] choices() RENDER find("recipient_option").on_click() assert(calls[2].args.target.to_json()==target.to_json())''',
    'next_only_native_row_is_selectable': '''options.items=[{id:"project-original" label:"Project" detail:"project-original" next:project_next}] choices() RENDER find("recipient_option").on_click() assert(calls[2].args.to_json()==project_next.to_json())''',
    'native_state_and_scope_are_localized': '''assert(A.recipient_option_detail({status:"approved"})==A.t("Agent request approved","Agent 申请已批准")) assert(A.recipient_option_detail({scopeKind:"owner_direct"})==A.t("Owner’s direct room","主人的专属房间")) assert(A.recipient_option_detail({scopeKind:"project"})==A.t("Project room","项目房间")) assert(A.recipient_option_detail({status:"internal_future_code"})==A.t("Current status unavailable","当前状态暂不可用"))''',
    'prepared_binding_does_not_claim_applied': '''binding_prepared.deliveryState="binding_saved" A.flow_prepared(binding_prepared) RENDER assert(has(A.t("Binding request saved; awaiting confirmation","绑定请求已保存，等待确认")) && !has(A.flow_delivery("binding_saved")))''',
    'previous_identity_details_start_collapsed': '''current.state="binding_saved" current.binding=snapshot enter() RENDER assert(find("recipient_current_json")==nil && has(snapshot.recipientMxid)) find("recipient_current_details").on_click() assert(H.flow_recipient.currentDetails)''',
    'observed_identity_details_start_collapsed': '''select_leaf() RENDER assert(find("recipient_identity_json")==nil && find("recipient_prepare")!=nil && count("recipient_option")==0) find("recipient_observation_details").on_click() assert(H.flow_recipient.observationDetails)''',
    'saved_identity_details_start_collapsed': '''A.flow_prepared(binding_prepared) RENDER assert(find("recipient_saved_json")==nil && has(A.t("Recipient Matrix ID","接收方 Matrix ID")+" · "+snapshot.recipientMxid)) find("recipient_saved_details").on_click() assert(H.flow_review_json)''',
    'change_selection_clears_observation_without_writing': '''select_leaf() RENDER find("recipient_change_choice").on_click() assert(H.flow_recipient.target==nil && H.flow_recipient.observation==nil && calls.len()==3 && H.flow_recipient.options==options)''',
    'new_observation_hides_previous_details': '''select_leaf() H.flow_recipient.observationDetails=true A.recipient_observe(target) assert(!H.flow_recipient.observationDetails && H.flow_recipient.observation==nil && calls[3].service=="hagency.graphs.recipient.observe")''',
    'all_recipient_blocking_reasons_are_translated': '''let reasons=["node_not_held","sender_not_joined","recipient_not_joined","room_identity_unavailable","sender_cannot_send","personal_binding_inactive","personal_binding_scope_unavailable","project_room_inactive","shared_project_manager_required","shared_agent_not_approved","shared_project_not_ready","shared_membership_unavailable"] for reason in reasons{assert(A.recipient_reason(reason)!=reason && A.recipient_reason(reason)!=A.error_text("unknown"))}''',
    'shared_manager_only_refusal_uses_native_code': '''observation.canBind=false observation.blockingReasons=["shared_project_manager_required"] select_leaf() RENDER assert(find("recipient_prepare")==nil && has(A.recipient_reason("shared_project_manager_required")))''',
    'encrypted_identity_is_metadata_only': '''observation.identityJson="encrypted room identity" select_leaf() RENDER assert(find("recipient_prepare")!=nil && has(A.recipient_notice()) && calls.len()==3)''',
})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('vm', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    source = (APP / 'bundle/main.splash').read_text()
    start = source.index('        if H.page=="operator_graphs" {')
    end = source.index('        if H.page == "operator_tasks" {', start)
    render = '\nlet tree=View{\n' + source[start:end] + '\n}\nvisit(tree)\n'
    results = []
    for language in ['en', 'zh-CN']:
        prefix = fixture(source + '\nmod.graph_workflow_actions=A\n', language)
        for name, body in CASES.items():
            script = args.output / f'{language}-{name}.splash'
            script.write_text(prefix + SETUP + RECIPIENT_SETUP + body.replace('RENDER', render) + '\ntrue\n')
            run = subprocess.run([str(args.vm.resolve()), 'eval', str(script)], text=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            script.with_suffix('.log').write_text(run.stdout)
            results.append({'case': name, 'language': language, 'passed': run.returncode == 0})
    (args.output / 'report.json').write_text(json.dumps({'checks': results}, indent=2) + '\n')
    print(json.dumps({'passed': sum(row['passed'] for row in results), 'total': len(results)}))
    raise SystemExit(not all(row['passed'] for row in results))


if __name__ == '__main__':
    main()
