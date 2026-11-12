#!/usr/bin/env python3
"""Exercise shipped canonical graph workflows with real Splash callbacks.

The full bundle is evaluated for each case. Widgets are inert; these checks do
not claim pixels, native persistence, agent delivery or end-to-end execution.
"""
import argparse
import json
import subprocess
from pathlib import Path
from check_full_bundle import fixture, APP

SETUP = r'''
let H=mod.hagency let A=mod.graph_workflow_actions
A.redraw=fn(){return nil} H.busy=false H.ready=true H.notice="" H.command=nil H.saved_commands=[]
let calls=[] let replies=[]
host.request=fn(service,args,done){calls.push({service:service args:args}) replies.push(done)}
let summary={id:"graph-original" revision:7 label:"Original workflow" owner:"@manager:example.test" status:"active" nodeCount:2 completeCount:1 deliveryState:"pending_binding" runtimeDeliveryAuthorized:false}
let list={supported:true graphs:[summary] nextAfter:"graph-original"}
let detail={graph:summary nodes:[{id:"first" assignee:"researcher" status:"complete" dependencyCount:0 deliveryState:"pending_binding"},{id:"second" assignee:"writer" status:"dispatched" dependencyCount:1 deliveryState:"pending_binding"}] nextAfter:nil}
let node={graphId:"graph-original" revision:7 id:"second" assignee:"writer" description:"Write the final report" status:"dispatched" dependsOn:["first"] result:{facts:[true,4,"keep"]} resultJson:"{\"facts\":[true,4,\"keep\"]}" conditionJson:"{\"dep\":\"first\",\"path\":\"approved\",\"eq\":true}" error:nil condition:{dep:"first" path:"approved" eq:true} dispatchedAt:nil startedAt:nil completedAt:nil deliveryState:"pending_binding" runtimeDeliveryAuthorized:false}
let original_change={operation:"create" label:"Original workflow" nodes:{first:{assignee:"researcher" description:"Read sources" dependsOn:[] condition:nil},second:{assignee:"writer" description:"Write the final report" dependsOn:["first"] condition:{dep:"first" path:"approved" eq:true}}}}
let prepared={commandId:"graphcmd_original" graphId:"graph-original" expectedRevision:0 operation:"graph_create" change:original_change result:nil error:nil deliveryState:"pending_binding" runtimeDeliveryAuthorized:false}
prepared.changeJson=original_change.to_json()
let result={commandId:"graphcmd_original" scopeId:"scope-original" graphId:"graph-original" revision:1 documentDigest:"digest" queuedMessages:1 suppressedMessages:0 deliveryState:"pending_binding" runtimeDeliveryAuthorized:false}
let widgets=[] let texts=[]
let visit=fn(n){if !n.is_object(){return} for index key child in n{widgets.push({name:key.to_string() value:child}) if key.to_string()=="text" && child.is_string(){texts.push(child)} visit(child)}}
let find=fn(name){for row in widgets{if row.name==name{return row.value}} return nil}
let count=fn(name){let total=0 for row in widgets{if row.name==name{total=total+1}} return total}
let has=fn(text){for value in texts{if value==text{return true}} return false}
let draft_node=fn(id){return {id:id assignee:"Recipient" description:"Do "+id dependsOn:[] conditionJson:""}}
let start_draft=fn(){A.flow_new() H.flow_draft.label="Original workflow" H.flow_draft.nodes=[draft_node("first"),draft_node("second")] H.flow_draft.nodes[1].dependsOn=["first"] H.flow_draft.nodes[1].conditionJson="{\"dep\":\"first\",\"path\":\"approved\",\"eq\":true}"}
'''

CASES = {
    'list_request_and_response': '''A.open_flows(nil) assert(H.busy && calls[0].service=="hagency.graphs.list") replies[0]({is_ok:true data:list}) assert(!H.busy && H.flow_list==list)''',
    'filtered_empty_page_preserves_cursor': '''H.page="operator_graphs" H.flow_list=list list.graphs=[] H.flow_filter="active" RENDER assert(has(A.t("No matching graphs on this page","本页没有匹配的任务图"))) find("flow_next").on_click() assert(calls[0].args.after=="graph-original" && calls[0].args.status=="active")''',
    'graph_navigation_uses_original_id': '''H.page="operator_graphs" H.flow_list=list RENDER find("flow_open").on_click() assert(calls[0].service=="hagency.graphs.get" && calls[0].args.id=="graph-original") replies[0]({is_ok:true data:detail}) assert(H.flow_detail==detail)''',
    'node_navigation_uses_original_id': '''H.page="operator_graph" H.flow_id="graph-original" H.flow_detail=detail RENDER let controls=[] for row in widgets{if row.name=="flow_node_open"{controls.push(row.value)}} controls[1].on_click() assert(calls[0].args.id=="graph-original" && calls[0].args.nodeId=="second") replies[0]({is_ok:true data:node}) assert(H.flow_node==node)''',
    'node_back_keeps_node_page': '''H.page="operator_graph_node" H.flow_node=node H.flow_id="graph-original" H.flow_node_after="cursor-original" RENDER find("flow_back").on_click() assert(calls[0].args.nodeAfter=="cursor-original")''',
    'graph_back_keeps_list_page': '''H.page="operator_graph" H.flow_detail=detail H.flow_after="list-cursor" RENDER find("flow_back").on_click() assert(calls[0].args.after=="list-cursor")''',
    'mismatched_graph_reply_refused': '''A.open_flow("graph-original",nil) detail.graph.id="other" replies[0]({is_ok:true data:detail}) assert(H.flow_detail==nil && H.notice!="")''',
    'mismatched_node_reply_refused': '''A.open_flow_node("graph-original","second") node.id="other" replies[0]({is_ok:true data:node}) assert(H.flow_node==nil && H.notice!="")''',
    'stale_page_reply_ignored': '''A.open_flow_node("graph-original","second") H.page="overview" replies[0]({is_ok:true data:node}) assert(H.page=="overview" && H.flow_node==nil && !H.busy)''',
    'stale_epoch_reply_ignored': '''A.open_flow_node("graph-original","second") H.flow_epoch=H.flow_epoch+1 H.busy=false A.open_flows(nil) replies[0]({is_ok:true data:node}) assert(H.busy && H.flow_node==nil) replies[1]({is_ok:true data:list}) assert(!H.busy && H.flow_list==list)''',
    'stale_target_reply_ignored': '''A.open_flow_node("graph-original","second") H.flow_node_id="first" replies[0]({is_ok:true data:node}) assert(H.flow_node==nil)''',
    'busy_blocks_reads_and_edits': '''H.busy=true A.open_flows(nil) A.open_flow("a",nil) A.open_flow_node("a","n") A.flow_new() assert(calls.len()==0 && H.flow_draft==nil)''',
    'unrelated_saved_command_preserved': '''let original={service:"other" args:{commandId:"preserve"}} H.command=original A.flow_new() A.open_flows(nil) replies[0]({is_ok:true data:list}) assert(H.command==original && H.flow_draft==nil)''',
    'pending_graph_request_blocks_replacement': '''H.saved_commands=[{operation:"graph_create" commandId:"original" hasReply:false}] A.flow_new() assert(H.flow_draft==nil && calls.len()==0)''',
    'recorded_status_is_never_delivery': '''H.page="operator_graph_node" H.flow_node=node RENDER assert(has(A.flow_notice()) && has(A.flow_status("dispatched")+" · "+A.flow_delivery("pending_binding"))) assert(!has("Dispatched") && !has("已派发"))''',
    'multi_node_and_condition_prepare_only': '''start_draft() A.flow_prepare_create() assert(calls.len()==1 && calls[0].service=="hagency.graphs.create") assert(calls[0].args.nodes["second"].dependsOn[0]=="first" && calls[0].args.nodes["second"].conditionJson==H.flow_draft.nodes[1].conditionJson) replies[0]({is_ok:true data:prepared}) assert(calls.len()==1 && H.flow_phase=="saved" && H.flow_pending.commandId=="graphcmd_original")''',
    'empty_create_stays_editable': '''A.flow_new() A.flow_prepare_create() assert(calls.len()==0 && H.flow_phase=="editing" && H.flow_error!="") H.flow_draft.label="Name" A.flow_prepare_create() assert(calls.len()==0)''',
    'node_editor_adds_unique_node': '''A.flow_new() A.flow_edit(-1) A.flow_set_node("id","alpha") A.flow_set_node("assignee","Agent A") A.flow_set_node("description","Research") A.flow_save_node() assert(H.flow_draft.nodes.len()==1 && H.flow_draft.nodes[0].id=="alpha" && H.page=="operator_graph_edit")''',
    'duplicate_node_refused': '''start_draft() A.flow_edit(-1) A.flow_set_node("id","first") A.flow_set_node("assignee","Agent") A.flow_set_node("description","Duplicate") A.flow_save_node() assert(H.flow_draft.nodes.len()==2 && H.page=="operator_graph_edit_node" && H.flow_error!="")''',
    'node_rename_retargets_dependencies': '''start_draft() A.flow_edit(0) A.flow_set_node("id","renamed") A.flow_save_node() assert(H.flow_draft.nodes[0].id=="renamed" && H.flow_draft.nodes[1].dependsOn[0]=="renamed")''',
    'remove_node_removes_dependencies': '''start_draft() A.flow_remove_node(0) assert(H.flow_draft.nodes.len()==1 && H.flow_draft.nodes[0].id=="second" && H.flow_draft.nodes[0].dependsOn.len()==0)''',
    'dependency_toggle_roundtrip': '''start_draft() A.flow_edit(1) A.flow_toggle_dep("first") assert(H.flow_edit_node.dependsOn.len()==0) A.flow_toggle_dep("first") A.flow_toggle_dep("second") assert(H.flow_edit_node.dependsOn.len()==1 && H.flow_edit_node.dependsOn[0]=="first")''',
    'self_dependency_refused': '''start_draft() A.flow_edit(1) H.flow_edit_node.dependsOn=["second"] A.flow_save_node() assert(H.page=="operator_graph_edit_node" && H.flow_error!="")''',
    'node_edit_cancel_keeps_original': '''start_draft() A.flow_edit(1) A.flow_set_node("description","Unsaved") RENDER find("flow_node_cancel").on_click() assert(H.flow_draft.nodes[1].description=="Do second" && H.page=="operator_graph_edit")''',
    'node_limit_blocks_257th': '''A.flow_new() for i in 0..256{H.flow_draft.nodes.push(draft_node("node_"+i.to_string()))} A.flow_edit(-1) assert(H.flow_edit_node==nil && H.page=="operator_graph_edit") RENDER assert(!find("flow_add_node").enabled && count("flow_edit_node")==16)''',
    'draft_pages_bound_to_16': '''A.flow_new() for i in 0..20{H.flow_draft.nodes.push(draft_node("node_"+i.to_string()))} RENDER assert(count("flow_edit_node")==16) find("flow_edit_next").on_click() assert(H.flow_editor_offset==16)''',
    'draft_last_page_has_four': '''A.flow_new() for i in 0..20{H.flow_draft.nodes.push(draft_node("node_"+i.to_string()))} H.flow_editor_offset=16 RENDER assert(count("flow_edit_node")==4 && find("flow_edit_next")==nil) find("flow_edit_previous").on_click() assert(H.flow_editor_offset==0)''',
    'dependency_choices_are_bounded': '''A.flow_new() for i in 0..20{H.flow_draft.nodes.push(draft_node("node_"+i.to_string()))} A.flow_edit(-1) RENDER assert(count("flow_dependency_choice")==16) find("flow_choice_next").on_click() assert(H.flow_choice_offset==16)''',
    'node_dependency_pages_are_bounded': '''H.page="operator_graph_node" H.flow_node=node node.dependsOn=[] for i in 0..20{node.dependsOn.push("node_"+i.to_string())} RENDER assert(count("flow_dependency")==16) find("flow_dep_next").on_click() assert(H.flow_dep_offset==16 && calls.len()==0)''',
    'native_json_rejection_keeps_draft_editable': '''start_draft() H.flow_draft.nodes[1].conditionJson="{" A.flow_prepare_create() replies[0]({is_ok:false error:"invalid_management_arguments"}) assert(H.flow_phase=="editing" && H.page=="operator_graph_edit" && H.flow_draft.nodes[1].conditionJson=="{" && calls.len()==1)''',
    'lost_prepare_does_not_prepare_replacement': '''start_draft() A.flow_prepare_create() replies[0]({is_ok:false error:"management_outcome_unknown"}) A.flow_prepare_create() A.flow_new() A.flow_apply() assert(calls.len()==1 && H.flow_phase=="unknown" && H.flow_pending==nil)''',
    'saved_restart_reviews_exact_original_without_apply': '''A.recover_native({commandId:"graphcmd_original" operation:"graph_create"}) assert(calls[0].service=="hagency.graphs.command.get") replies[0]({is_ok:true data:prepared}) assert(H.flow_pending.change==original_change && H.flow_phase=="saved" && calls.len()==1)''',
    'wrong_saved_command_refused': '''A.open_saved_graph({commandId:"expected"}) replies[0]({is_ok:true data:prepared}) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'apply_uses_only_original_native_id': '''A.flow_prepared(prepared) RENDER find("flow_command_apply").on_click() A.flow_apply() assert(calls.len()==1 && calls[0].service=="hagency.graphs.command.apply" && calls[0].args.to_json()=={commandId:"graphcmd_original"}.to_json()) replies[0]({is_ok:true data:result}) assert(H.flow_phase=="recorded") A.flow_apply() assert(calls.len()==1)''',
    'unknown_apply_requires_result_check_in_ui': '''A.flow_prepared(prepared) A.flow_apply() replies[0]({is_ok:false error:"management_outcome_unknown"}) RENDER assert(find("flow_command_apply")==nil && find("flow_command_check")!=nil && H.flow_pending.commandId=="graphcmd_original") find("flow_command_check").on_click() assert(calls[1].service=="hagency.graphs.command.get" && calls[1].args.commandId=="graphcmd_original")''',
    'recovery_get_does_not_auto_apply': '''A.flow_prepared(prepared) A.flow_apply() replies[0]({is_ok:false error:"management_outcome_unknown"}) A.flow_check_saved() replies[1]({is_ok:true data:prepared}) assert(H.flow_phase=="saved" && calls.len()==2) A.flow_apply() assert(calls[2].args.commandId=="graphcmd_original")''',
    'mismatched_apply_receipt_is_unknown': '''A.flow_prepared(prepared) A.flow_apply() result.graphId="other" replies[0]({is_ok:true data:result}) assert(H.flow_phase=="unknown" && H.flow_pending.result==nil)''',
    'late_apply_does_not_replace_new_view': '''A.flow_prepared(prepared) A.flow_apply() H.page="overview" replies[0]({is_ok:true data:result}) assert(H.page=="overview" && H.flow_pending.result==nil)''',
    'node_update_keeps_result_when_omitted': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.changeStatus=true H.flow_update.status="complete" A.flow_prepare_update() assert(calls[0].args.expectedRevision==7 && calls[0].args.patch.to_json()=={nodeId:"second" status:"complete"}.to_json())''',
    'node_update_preserves_arbitrary_json_text': r'''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.resultMode="set" H.flow_update.resultJson="[true,4,\"exact\",null,{\"nested\":false}]" A.flow_prepare_update() assert(calls[0].args.patch.resultJson==H.flow_update.resultJson && calls[0].args.patch.nodeId=="second")''',
    'explicit_null_result_is_not_omitted': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.resultMode="set" H.flow_update.resultJson="null" A.flow_prepare_update() assert(calls[0].args.patch.to_json()=={nodeId:"second" resultJson:"null"}.to_json())''',
    'clear_error_is_explicit_null': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.errorMode="set" H.flow_update.error="" A.flow_prepare_update() assert(calls[0].args.patch.to_json()=={nodeId:"second" error:nil}.to_json())''',
    'unchanged_node_has_no_request': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() A.flow_prepare_update() assert(calls.len()==0 && H.flow_phase=="editing" && H.flow_error!="")''',
    'blank_json_has_no_request': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.resultMode="set" H.flow_update.resultJson=" " A.flow_prepare_update() assert(calls.len()==0 && H.flow_error!="")''',
    'node_update_widgets_keep_modes': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() RENDER assert(count("flow_status_choice")==7) find("flow_result_set").on_click() find("flow_error_set").on_click() assert(H.flow_update.resultMode=="set" && H.flow_update.errorMode=="set") find("flow_result_keep").on_click() assert(H.flow_update.resultMode=="keep")''',
    'cancel_prepares_exact_revision_without_apply': '''H.flow_detail=detail A.flow_prepare_cancel() assert(calls.len()==1 && calls[0].service=="hagency.graphs.cancel" && calls[0].args.expectedRevision==7 && calls[0].args.id=="graph-original") prepared.operation="graph_cancel" prepared.change={operation:"cancel"} prepared.expectedRevision=7 prepared.deliveryState="suppressed" replies[0]({is_ok:true data:prepared}) assert(H.flow_phase=="saved" && calls.len()==1)''',
    'cancelled_graph_has_no_cancel_control': '''H.page="operator_graph" H.flow_detail=detail summary.status="cancelled" RENDER assert(find("flow_cancel")==nil) A.flow_prepare_cancel() assert(calls.len()==0)''',
    'saved_review_is_parsed_exact_data': '''A.flow_prepared(prepared) RENDER assert(has("Original workflow") && has("Write the final report") && has(A.t("Condition recorded · Open exact saved JSON to inspect it.","已记录条件 · 可展开保存的完整 JSON 核对。")))''',
    'review_nodes_bounded': '''prepared.change.nodes={} for i in 0..20{prepared.change.nodes["node_"+i.to_string()]={assignee:"Agent" description:"Description" dependsOn:[] condition:nil}} A.flow_prepared(prepared) RENDER find("flow_review_next").on_click() assert(H.flow_review_offset==16 && calls.len()==0)''',
    'recorded_receipt_closes_only_matching_saved_command': '''prepared.result=result A.flow_prepared(prepared) H.saved_commands=[{commandId:"keep" operation:"task_comment"},{commandId:"graphcmd_original" operation:"graph_create"}] A.flow_close_result() assert(calls[0].service=="hagency.intent.acknowledge" && calls[0].args.commandId=="graphcmd_original") replies[0]({is_ok:true data:{}}) assert(H.saved_commands.len()==1 && H.saved_commands[0].commandId=="keep" && H.flow_pending==nil && calls[1].service=="hagency.graphs.get")''',
    'failed_ack_retains_exact_receipt': '''prepared.result=result A.flow_prepared(prepared) A.flow_close_result() replies[0]({is_ok:false error:"unavailable"}) assert(H.flow_pending.commandId=="graphcmd_original" && H.flow_phase=="recorded" && calls.len()==1)''',
    'failed_saved_command_is_reviewable': '''prepared.error="management_revision_conflict" A.flow_prepared(prepared) RENDER assert(H.flow_phase=="failed" && find("flow_command_apply")==nil && find("flow_command_close")!=nil)''',
}


CASES.update({
    'unicode_node_character_limits_are_accepted': '''A.flow_new() A.flow_edit(-1) let short="" let long="" for i in 0..255{short=short+"节"} for i in 0..4000{long=long+"述"} A.flow_set_node("id",short) A.flow_set_node("assignee",short) A.flow_set_node("description",long) A.flow_save_node() assert(H.flow_draft.nodes.len()==1 && H.flow_draft.nodes[0].description==long && H.flow_error=="")''',
    'unicode_node_character_overflow_is_rejected': '''A.flow_new() A.flow_edit(-1) let short="" let long="" for i in 0..256{short=short+"节"} for i in 0..4001{long=long+"述"} A.flow_set_node("id",short) A.flow_set_node("assignee","Agent") A.flow_set_node("description","Description") A.flow_save_node() assert(H.flow_draft.nodes.len()==0) A.flow_set_node("id","node") A.flow_set_node("assignee",short) A.flow_save_node() assert(H.flow_draft.nodes.len()==0) A.flow_set_node("assignee","Agent") A.flow_set_node("description",long) A.flow_save_node() assert(H.flow_draft.nodes.len()==0 && H.flow_error!="")''',
    'unicode_graph_label_at_character_limit_is_accepted': '''start_draft() let label="" for i in 0..4000{label=label+"图"} H.flow_draft.label=label A.flow_prepare_create() assert(calls.len()==1 && calls[0].args.label==label)''',
    'unicode_graph_label_over_limit_is_rejected': '''start_draft() let label="" for i in 0..4001{label=label+"图"} H.flow_draft.label=label A.flow_prepare_create() assert(calls.len()==0 && H.flow_error!="")''',
    'unicode_condition_keeps_byte_limit': '''A.flow_new() A.flow_edit(-1) A.flow_set_node("id","node") A.flow_set_node("assignee","Agent") A.flow_set_node("description","Description") let json="" for i in 0..22000{json=json+"条"} A.flow_set_node("conditionJson",json) A.flow_save_node() assert(H.flow_draft.nodes.len()==0 && H.flow_error!="")''',
    'native_exact_json_prefills_without_vm_serialization': r'''H.flow_id="graph-original" H.flow_node=node node.resultJson="{\"large\":9007199254740993,\"deep\":[[[[[[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]]]]]]}" A.flow_begin_update() assert(H.flow_update.resultJson==node.resultJson) H.flow_update.resultMode="set" A.flow_prepare_update() assert(calls[0].args.patch.resultJson==node.resultJson)''',
    'saved_exact_json_is_read_only_and_unmodified': r'''prepared.changeJson="{\"large\":9007199254740993,\"condition\":{\"nested\":[false,null,\"unchanged\"]}}" A.flow_prepared(prepared) H.flow_review_json=true RENDER assert(find("flow_saved_json").text==prepared.changeJson && find("flow_saved_json").is_read_only)''',
    'revision_conflict_during_prepare_keeps_update_draft': '''H.flow_id="graph-original" H.flow_node=node A.flow_begin_update() H.flow_update.changeStatus=true H.flow_update.status="complete" A.flow_prepare_update() replies[0]({is_ok:false error:"management_revision_conflict"}) assert(H.flow_phase=="editing" && H.page=="operator_graph_update" && H.flow_update.expectedRevision==7)''',
    'cancel_preflight_failure_returns_graph': '''H.flow_detail=detail A.flow_prepare_cancel() replies[0]({is_ok:false error:"management_revision_conflict"}) assert(H.page=="operator_graph" && H.flow_phase=="" && H.notice!="" && H.flow_pending==nil)''',
    'oversize_read_page_refused': '''A.open_flows(nil) list.graphs=[] for i in 0..17{list.graphs.push(summary)} replies[0]({is_ok:true data:list}) assert(H.flow_list==nil && H.notice!="")''',
    'runtime_authority_injected_reply_refused': '''A.open_flow_node("graph-original","second") node.runtimeDeliveryAuthorized=true replies[0]({is_ok:true data:node}) assert(H.flow_node==nil && H.notice!="")''',
    'saved_variant_mismatch_refused': '''prepared.operation="graph_cancel" A.flow_prepared(prepared) assert(H.flow_pending==nil && H.flow_phase=="unknown")''',
    'node_json_fields_use_exact_native_text': '''H.page="operator_graph_node" H.flow_node=node RENDER assert(find("flow_node_condition_json").text==node.conditionJson && find("flow_node_result_json").text==node.resultJson && find("flow_node_result_json").is_read_only)''',
    'mismatched_saved_receipt_is_not_acknowledged': '''prepared.result=result result.graphId="other" A.flow_prepared(prepared) A.flow_close_result() assert(H.flow_pending==nil && H.flow_phase=="unknown" && calls.len()==0)''',
    'stale_status_filter_reply_ignored': '''A.open_flows(nil) H.flow_filter="failed" replies[0]({is_ok:true data:list}) assert(H.flow_list==nil)''',
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
            script.write_text(prefix + SETUP + body.replace('RENDER', render) + '\ntrue\n')
            run = subprocess.run([str(args.vm.resolve()), 'eval', str(script)], text=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            script.with_suffix('.log').write_text(run.stdout)
            results.append({'case': name, 'language': language, 'passed': run.returncode == 0})
    (args.output / 'report.json').write_text(json.dumps({'checks': results}, indent=2) + '\n')
    print(json.dumps({'passed': sum(x['passed'] for x in results), 'total': len(results)}))
    raise SystemExit(not all(x['passed'] for x in results))


if __name__ == '__main__':
    main()
