#!/usr/bin/env python3
"""The server administrator's admission review: states, filter and the count that waits."""
import argparse
import json
from pathlib import Path
import subprocess
from check_full_bundle import APP, fixture

SETUP = '''
H.redraws=0 H.tab_renders=0
A.redraw=fn(){H.redraws=H.redraws+1}
A.save=fn(){return true}
let ui={navigation:{render:fn(){H.tab_renders=H.tab_renders+1}}}
H.ready=true H.busy=false H.command=nil H.saved_commands=[] H.needs_signin=false
H.session={account:"@admin:test" canAdmitResources:true features:[]}
H.entry_role="admin" H.page="inbox"
H.calls=[] H.quiet=[]
A.request=fn(service,args,done){H.calls.push({service:service args:args}) H.callback=done}
host.request=fn(service,args,done){H.quiet.push({service:service args:args}) H.quiet_callback=done}
let req=fn(id,state){return {id:id kind:"resource_admission" contributionId:"con_"+id resourceLabel:"Pool "+id requesterMxid:"@owner:test" approverMxid:"@admin:test" state:state revision:1 createdAtMs:1000 expiresAtMs:2000 decidedAtMs:nil canDecide:state=="pending"}}
'''
CASES = {
    "one_read_lists_every_state_newest_first": '''
A.load_inbox()
assert(H.calls.len()==1 && H.calls[0].service=="hagency.inbox.list")
assert(!H.calls[0].args.needsMyAction && !H.calls[0].args.includeProjects && !H.calls[0].args.includeAllocations && H.calls[0].args.after==nil)
assert(!H.adm.ready && !H.ux.inboxReady)
H.callback({items:[req("a","approved"),req("b","revoked"),req("c","rejected"),req("d","expired"),req("e","pending"),req("f","pending")] nextAfter:nil})
assert(H.calls.len()==1 && H.adm.ready && H.ux.inboxReady && !H.adm.partial)
assert(H.adm.rows.len()==6 && H.adm.rows[0].id=="f" && H.adm.rows[5].id=="a")
assert(A.adm_count("pending")==2 && A.adm_count("approved")==1 && A.adm_count("rejected")==1 && A.adm_count("revoked")==1 && A.adm_count("expired")==1 && A.adm_count("all")==6)
assert(H.adm.pending==2 && !H.adm.pendingMore && A.tab_badge("inbox")==2 && A.tab_badge("resources")==0)
// With requests that wait, they are what the page opens on.
assert(H.adm.filter=="auto" && A.adm_shown()=="pending" && A.adm_rows().len()==2 && A.adm_rows()[0].id=="f")
A.adm_filter("revoked") assert(A.adm_shown()=="revoked" && A.adm_rows().len()==1 && A.adm_rows()[0].id=="b")
A.adm_filter("all") assert(A.adm_rows().len()==6)
// A filter is a view of what was read: no further request.
assert(H.calls.len()==1)
''',
    "nothing_waiting_opens_on_all": '''
A.load_inbox()
H.callback({items:[req("a","approved"),req("b","revoked")] nextAfter:nil})
assert(A.adm_shown()=="all" && A.adm_rows().len()==2 && H.adm.pending==0 && A.tab_badge("inbox")==0)
A.adm_filter("pending") assert(A.adm_rows().len()==0)
''',
    "state_words": '''
assert(A.adm_state_text("pending")==A.t("Needs review","待审批"))
assert(A.adm_state_text("approved")==A.t("Approved","已批准"))
assert(A.adm_state_text("rejected")==A.t("Declined","已拒绝"))
assert(A.adm_state_text("revoked")==A.t("Closed","已关闭"))
assert(A.adm_state_text("expired")==A.t("Expired","已过期"))
assert(A.adm_filter_text("all")==A.t("All","全部"))
''',
    "pages_are_followed": '''
A.load_inbox()
H.callback({items:[req("a","approved"),req("b","pending")] nextAfter:"b"})
assert(H.calls.len()==2 && H.calls[1].args.after=="b" && !H.calls[1].args.needsMyAction && !H.adm.ready)
H.callback({items:[req("c","rejected")] nextAfter:nil})
assert(H.adm.ready && !H.adm.partial && H.adm.rows.len()==3 && H.adm.rows[0].id=="c" && H.adm.rows[2].id=="a" && H.adm.pending==1)
''',
    "a_cut_list_still_has_every_waiting_request": '''
A.load_inbox()
// Sixteen pages, and the server still has more.
for page in 16 {
    assert(H.calls.len()==page+1 && !H.calls[page].args.needsMyAction)
    H.callback({items:[req("old"+page,"approved")] nextAfter:"old"+page})
}
// The seventeenth request asks only for the ones that wait.
assert(H.calls.len()==17 && H.calls[16].args.needsMyAction && H.calls[16].args.after==nil && !H.adm.ready)
H.callback({items:[req("new1","pending"),req("new2","pending")] nextAfter:nil})
assert(H.adm.ready && H.adm.partial && H.adm.rows.len()==18)
assert(H.adm.rows[0].id=="new2" && H.adm.rows[1].id=="new1" && H.adm.rows[2].id=="old15" && H.adm.pending==2)
''',
    "an_overtaken_read_is_dropped": '''
A.load_inbox() let first=H.callback
A.load_inbox()
first({items:[req("stale","pending")] nextAfter:nil})
assert(!H.adm.ready && H.adm.rows.len()==0)
H.callback({items:[req("a","approved")] nextAfter:nil})
assert(H.adm.ready && H.adm.rows.len()==1 && H.adm.rows[0].id=="a")
''',
    "the_open_page_takes_in_new_requests": '''
A.load_inbox()
H.callback({items:[req("a","approved"),req("b","pending")] nextAfter:nil})
let plan=A.web_refresh_plan()
assert(plan.service=="hagency.inbox.list" && plan.args.needsMyAction && !plan.args.includeProjects && plan.args.after==nil)
plan.done({items:[req("b","pending"),req("c","pending")] nextAfter:nil})
assert(H.calls.len()==1 && H.adm.rows.len()==3 && H.adm.rows[0].id=="c" && H.adm.rows[1].id=="b" && H.adm.rows[2].id=="a" && H.adm.pending==2)
// One that no longer waits was decided elsewhere: the list is read again.
plan.done({items:[req("c","pending")] nextAfter:nil})
assert(H.calls.len()==2 && !H.calls[1].args.needsMyAction && !H.adm.ready)
H.callback({items:[req("a","approved"),req("b","approved"),req("c","pending")] nextAfter:nil})
assert(H.adm.ready && H.adm.rows.len()==3 && H.adm.pending==1 && A.adm_count("approved")==2)
// Nothing is planned while the list is being read.
A.load_inbox() assert(A.web_refresh_plan()==nil)
''',
    "the_count_keeps_up_on_other_pages": '''
H.page="connections"
A.tab_counts_load()
assert(H.quiet.len()==1 && H.quiet[0].service=="hagency.inbox.list" && H.quiet[0].args.needsMyAction && !H.quiet[0].args.includeProjects)
H.quiet_callback({is_ok:true data:{items:[req("a","pending"),req("b","pending"),req("c","pending")] nextAfter:nil}})
assert(H.adm.pending==3 && A.tab_badge("inbox")==3 && H.tab_renders==1 && H.redraws==0)
// The same answer again draws nothing.
A.tab_counts_load() H.quiet_callback({is_ok:true data:{items:[req("a","pending"),req("b","pending"),req("c","pending")] nextAfter:nil}})
assert(H.tab_renders==1)
// A failed read keeps the last count.
A.tab_counts_load() H.quiet_callback({is_ok:false error:"offline"})
assert(H.adm.pending==3)
// More than one read returns: the count says so.
A.tab_counts_load() H.quiet_callback({is_ok:true data:{items:[req("a","pending")] nextAfter:"a"}})
assert(H.adm.pending==1 && H.adm.pendingMore && H.tab_renders==2)
// The home page shows the count itself.
H.page="overview" A.tab_counts_load() H.quiet_callback({is_ok:true data:{items:[] nextAfter:nil}})
assert(H.adm.pending==0 && !H.adm.pendingMore && H.redraws==1)
''',
    "only_an_account_that_decides_admission_reads_it": '''
H.entry_role="manager" H.page="overview"
H.session.canAdmitResources=false
A.adm_pending_load() assert(H.quiet.len()==0 && A.tab_badge("inbox")==0)
// The administrator's account reads it in any workspace: the chooser's card shows the count,
// the other workspaces' tabs do not.
H.session.canAdmitResources=true A.adm_pending_load() assert(H.quiet.len()==1)
H.quiet_callback({is_ok:true data:{items:[req("a","pending")] nextAfter:nil}})
assert(H.adm.pending==1 && A.tab_badge("inbox")==0)
H.entry_role="admin" assert(A.tab_badge("inbox")==1)
// An answer that arrives after the account changed is ignored.
A.adm_pending_load()
H.session={account:"@other:test" canAdmitResources:true features:[]}
H.quiet_callback({is_ok:true data:{items:[] nextAfter:nil}})
assert(H.adm.pending==1)
''',
    "the_console_counts_follow_the_tab": '''
H.page="tools"
A.refresh_page()
assert(H.calls.len()==1 && !H.calls[0].args.needsMyAction)
H.callback({items:[req("a","approved"),req("b","pending")] nextAfter:nil})
assert(H.adm.ready && A.adm_count("pending")==1 && H.adm.pending==1)
// The quiet read finds a second one: the full list is read again.
A.adm_pending_load()
H.quiet_callback({is_ok:true data:{items:[req("b","pending"),req("c","pending")] nextAfter:nil}})
assert(H.adm.pending==2 && H.calls.len()==2 && !H.calls[1].args.needsMyAction)
H.callback({items:[req("a","approved"),req("b","pending"),req("c","pending")] nextAfter:nil})
assert(A.adm_count("pending")==2 && A.adm_count("approved")==1)
''',
    "coming_back_from_a_request_keeps_the_state_shown": '''
A.load_inbox()
H.callback({items:[req("a","approved"),req("b","pending")] nextAfter:nil})
A.adm_filter("approved")
H.ux.origin="inbox" H.page="action"
A.return_origin()
assert(H.page=="inbox" && H.adm.filter=="approved" && H.calls.len()==2 && !H.calls[1].args.needsMyAction)
H.callback({items:[req("a","approved"),req("b","approved")] nextAfter:nil})
assert(A.adm_rows().len()==2 && H.adm.pending==0 && A.tab_badge("inbox")==0)
''',
    "entering_the_workspace_reads_the_count": '''
H.entry_role="" H.page="overview"
A.entry_choose("admin")
assert(H.entry_role=="admin" && H.quiet.len()==1 && H.quiet[0].args.needsMyAction)
H.quiet_callback({is_ok:true data:{items:[req("a","pending")] nextAfter:nil}})
assert(H.adm.pending==1 && A.tab_badge("inbox")==1)
''',
    "the_tab_starts_again_on_what_waits": '''
A.load_inbox()
H.callback({items:[req("a","approved"),req("b","pending")] nextAfter:nil})
A.adm_filter("approved") assert(A.adm_shown()=="approved")
A.refresh=fn(){} A.local_leaving=fn(page){return false}
A.navigate("inbox") assert(H.adm.filter=="auto" && A.adm_shown()=="pending")
// From the console's counts the page opens on the chosen state.
H.page="tools" A.adm_open("revoked") assert(H.page=="inbox" && H.adm.filter=="revoked")
''',
}

def main():
    p=argparse.ArgumentParser();p.add_argument("vm",type=Path);p.add_argument("--output",type=Path,required=True);a=p.parse_args()
    a.output.mkdir(parents=True,exist_ok=True)
    source=(APP/"bundle/main.splash").read_text();results=[]
    for language in ("en","zh-CN"):
        for name,case in CASES.items():
            path=a.output/f"{language}-{name}.splash";path.write_text(fixture(source+SETUP+case,language))
            r=subprocess.run([str(a.vm.resolve()),"root",str(path)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            path.with_suffix(".log").write_text(r.stdout);results.append(dict(language=language,case=name,passed=r.returncode==0))
    (a.output/"report.json").write_text(json.dumps(results,indent=2)+"\n")
    print(json.dumps(dict(passed=sum(r["passed"] for r in results),total=len(results))))
    raise SystemExit(not all(r["passed"] for r in results))

if __name__=="__main__": main()
