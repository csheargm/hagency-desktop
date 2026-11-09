#!/usr/bin/env python3
"""Retained Tasks/Board full-bundle VM controls; no live records or pixel claims."""
import argparse,json,subprocess
from pathlib import Path
from check_full_bundle import fixture,APP
SETUP='''
let H=mod.hagency let A=mod.task_actions
A.redraw=fn(){return nil} H.busy=false H.ready=true H.notice=""
let original={service:"original" args:{commandId:"preserve"}} H.command=original
let calls=[] A.request=fn(service,args,done){calls.push({service:service args:args})}
'''
CASES={
'filters_and_original_command': '''
H.task_filters={assignee:"Original agent" status:"blocked" priority:"p1" label:"needs review"}
A.open_tasks(16)
assert(calls[0].service=="hagency.tasks.list" && calls[0].args.assignee=="Original agent" && calls[0].args.offset==16 && calls[0].args.label=="needs review")
A.open_task("task_original",2)
assert(calls[1].service=="hagency.tasks.get" && calls[1].args.id=="task_original" && calls[1].args.commentOffset==2)
assert(H.command==original && H.task_filters.status=="blocked" && H.task_offset==16)
''',
'board_uses_original_assignee_without_execution': '''
A.open_board({projectId:"project_original" agentAfter:"Agent A"})
assert(calls[0].service=="hagency.tasks.board" && calls[0].args.projectId=="project_original")
A.agent_tasks("Original agent")
assert(calls[1].service=="hagency.tasks.list" && calls[1].args.assignee=="Original agent")
assert(H.command==original)
'''
}
def main():
 p=argparse.ArgumentParser();p.add_argument('vm',type=Path);p.add_argument('--bundle',type=Path,default=APP/'bundle/main.splash');p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True);source=a.bundle.read_text()
 start=source.index('        if H.page == "operator_tasks" {mod.widgets.HagencyDetailCard{');end=source.index('\n        if H.page ==',start);section=source[start:end]
 CASES['all_statuses_and_distinct_tasks_render']= '''
H.page="operator_tasks" H.task_filters_open=true H.task_page={supported:true tasks:[{id:"task_a" title:"A" status:"created" priority:"p2" assignee:nil updatedAt:"old"},{id:"task_b" title:"B" status:"blocked" priority:"p1" assignee:"Agent" updatedAt:"later"}] nextOffset:nil}
let opens=[] let states=[] let visit=fn(node){if !node.is_object(){return} for i key child in node {if key.to_string()=="task_open"{opens.push(child)} if key.to_string()=="task_status"{states.push(child)} visit(child)}}
let root=View{'''+section+'''} visit(root)
assert(opens.len()==2 && states.len()==6)
opens[0].on_click() opens[1].on_click() states[3].on_click()
assert(calls[0].args.id=="task_a" && calls[1].args.id=="task_b" && calls[2].args.status=="in_progress")
assert(H.command==original)
'''
 CASES['adopted_unavailable_has_no_fake_empty_board_or_editor']='''
H.page="operator_tasks" H.task_page={supported:false reason:"current_task_proof_required" tasks:[] nextOffset:nil}
let edits=[] let visit=fn(node){if !node.is_object(){return} for i key child in node{if key.to_string()=="task_open" || key.to_string()=="task_apply" || key.to_string()=="task_status"{edits.push(child)} visit(child)}}
let root=View{'''+section+'''} visit(root)
assert(edits.len()==0 && calls.len()==0 && H.command==original)
'''
 detail_start=source.index('        if H.page == "operator_task" {mod.widgets.HagencyDetailCard{');detail=source[detail_start:source.index('\n        if H.page ==',detail_start)]
 CASES['detail_preserves_comments_parent_and_filter_callbacks']='''
H.page="operator_task" H.task_id="task_original" H.task_comment_offset=0
H.task_detail={supported:true commentCount:3 nextCommentOffset:2 task:{id:"task_original" title:"Original task" description:"Original description" status:"blocked" priority:"p1" granularity:"task" assignee:"Original agent" created_by:"Unverified original author" created_at:"2026-10-01T00:00:00Z" updated_at:"2026-10-02T00:00:00Z" started_at:nil completed_at:nil heartbeat_at:nil waiting_reason:"Original waiting reason" waiting_until:nil parent_id:"task_parent" labels:["one","two"] comments:[{author:"Author A" text:"Comment A" ts:"old"},{author:"Author B" text:"Comment B" ts:"later"}]}}
let labels=[] let parents=[] let more=[] let visit=fn(node){if !node.is_object(){return}  for i key child in node{if key.to_string()=="task_label_filter"{labels.push(child)} if key.to_string()=="task_parent"{parents.push(child)} if key.to_string()=="task_comments_next"{more.push(child)} visit(child)}}
let root=View{'''+detail+'''} visit(root)
assert(labels.len()==2 && parents.len()==1 && more.len()==1)
parents[0].on_click() labels[1].on_click()
assert(calls[0].args.id=="task_parent" && calls[1].args.label=="two")
H.task_id="task_original" H.task_detail={nextCommentOffset:2} more[0].on_click()
assert(calls[2].args.id=="task_original" && calls[2].args.commentOffset==2 && H.command==original)
'''
 results=[]
 for lang in ['en','zh-CN']:
  for name,test in CASES.items():
   path=a.output/f'{lang}-{name}.splash';path.write_text(fixture(source+'\nmod.task_actions=A\n',lang)+SETUP+test+'\ntrue\n');r=subprocess.run([str(a.vm.resolve()),'eval',str(path)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True);path.with_suffix('.log').write_text(r.stdout);results.append({'language':lang,'case':name,'passed':r.returncode==0})
 (a.output/'report.json').write_text(json.dumps({'checks':results},indent=2)+'\n');print(json.dumps({'passed':sum(r['passed'] for r in results),'total':len(results)}));raise SystemExit(not all(r['passed'] for r in results))
if __name__=='__main__':main()
