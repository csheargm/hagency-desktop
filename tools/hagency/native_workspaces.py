#!/usr/bin/env python3
"""Role entry/switch/retention in the offline native host; no live mutations."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from native_pool_application import App, ROOT

class Workspaces(App):
    def __init__(self, output, binary, language, narrow, admin=True, restart=False):
        self.out=output;self.profile=output/"profile";self.trace=[]
        (self.profile/"sandbox").mkdir(parents=True,exist_ok=restart)
        (self.profile/"ui-language.json").write_text(json.dumps(language))
        self.state_path=self.profile/"sandbox/onboarding-v1.json"
        self.fixture_path=output/"fixtures.json"
        if not restart:
            self.fixture_path.write_text(json.dumps({
                "hagency.session":dict(protocol="hagency-management-v1",account="@reviewer:example.test",administrator="@admin:example.test",device="offline",deviceGeneration=1,canAdmitResources=admin,features=["resource-pool-agents-v1"]),
                "hagency.intent.pending":{"commands":[]},
                "hagency.pools.list":{"items":[],"nextAfter":None},
                "hagency.pools.agents.list":{"items":[],"nextAfter":None},
                "hagency.inbox.list":{"items":[],"nextAfter":None},
                "hagency.resources.list":{"resources":[],"nextAfter":None},
                "hagency.accounts.list":{"protocol":"hagency-management-v1","accounts":[],"account":None,"resources":[],"nextAfter":None},
                "hagency.connections.list":{"connections":[],"nextAfter":None},
            }))
        with socket.socket() as sock:sock.bind(("127.0.0.1",0));port=sock.getsockname()[1]
        self.base=f"http://127.0.0.1:{port}";self.log_path=output/("restart.log" if restart else "native.log");self.log=self.log_path.open("w")
        self.trace_name="restart-input-trace.json" if restart else "input-trace.json"
        self.process=subprocess.Popen([str(binary),f"--remote={port}"]+(["--narrow"] if narrow else []),cwd=ROOT,stdout=self.log,stderr=subprocess.STDOUT,
            env=dict(os.environ,HAGENCY_DESKTOP_DATA_DIR=str(self.profile),HAGENCY_UX_FIXTURES=str(self.fixture_path),MAKEPAD_HIDE_WINDOWS="1",MAKEPAD_NO_FOCUS="1"))
        self.wait(lambda:self.req("/s")["pid"]==self.process.pid)
        self.wait(lambda: self.row("entry_switch" if restart else "entry_manager")["r"][2]>0)

    def visible(self,name):
        width,height=self.req("/s")["w"][0]["sz"]
        fixed=name in {"entry_switch","refresh","overview","agents","projects","resources","accounts","inbox","devices","servers","work"}
        top=0 if fixed else self.row("content")["r"][1]+4
        for attempt in range(30):
            row=self.row(name);x,y,w,h=row["r"]
            if w>0 and h>0 and y>=top and y+h<=height-20:
                assert x>=0 and x+w<=width,(name,row)
                return row
            self.req("/m",k="scroll",x=width/2,y=height-100,dy=-10000 if attempt==0 and h==0 else (-220 if h>0 and y<top else 220),wait=1)
        raise AssertionError("Unreachable control "+name)

    def nav(self):
        return {r["i"] for r in self.snap() if r.get("i") in {"overview","agents","projects","resources","accounts","inbox","devices","servers","work"} and r["r"][2]>0 and r["r"][3]>0}

    def run(self,admin):
        assert not self.nav()
        assert self.row("entry_admin")["enabled"]==admin
        self.shot("01-entry")
        self.click("entry_manager");self.wait(lambda:self.nav()=={"overview","agents","projects","work"})
        assert json.loads(self.state_path.read_text())["workspace"]=="manager"
        self.shot("02-manager")
        self.click("work");self.wait(lambda:self.row("manager_work_tools")["r"][3]>0)
        assert self.row("console_capabilities")["r"][3]==0
        self.shot("03-manager-work")
        self.click("entry_switch");self.click("entry_owner")
        self.wait(lambda:self.nav()=={"resources","accounts","inbox"})
        self.req("/event",data="ux:dark");time.sleep(.3);self.shot("04-owner-dark")
        self.click("inbox");self.wait(lambda:self.row("owner_pending")["r"][2]>0)
        assert not any("Inbox could not" in r.get("t","") or "待办尚未完整" in r.get("t","") for r in self.snap())
        self.click("owner_history");self.shot("05-owner-approvals")
        if admin:
            self.click("entry_switch");self.click("entry_admin")
            self.wait(lambda:self.nav()=={"overview","inbox","servers","work"})
            self.req("/event",data="ux:violet");time.sleep(.3);self.shot("06-admin-violet")
            self.click("inbox");self.wait(lambda:any("No admission requests yet" in r.get("t","") or "还没有资源加入申请" in r.get("t","") for r in self.snap()))
            self.shot("07-admin-admission")
        self.click("entry_switch");self.click("entry_manager")
        self.wait(lambda:self.nav()=={"overview","agents","projects","work"})

def main():
    p=argparse.ArgumentParser();p.add_argument("--output",type=Path,required=True);p.add_argument("--binary",type=Path,default=ROOT/"target/release/examples/hagency_visual");a=p.parse_args()
    a.output.mkdir(parents=True,exist_ok=True);results=[]
    for lang,narrow,admin in [("en",False,True),("zh-CN",False,True),("en",True,True),("zh-CN",True,False)]:
        out=a.output.resolve()/(lang+("-narrow" if narrow else "-wide"))
        app=Workspaces(out,a.binary.resolve(),lang,narrow,admin)
        try:app.run(admin)
        except Exception:app.shot("failure");raise
        finally:app.stop()
        app=Workspaces(out,a.binary.resolve(),lang,narrow,admin,restart=True)
        try:
            app.wait(lambda:app.nav()=={"overview","agents","projects","work"});app.shot("08-retained-workspace")
            assert json.loads(app.state_path.read_text())["workspace"]=="manager"
        finally:app.stop()
        results.append(dict(language=lang,narrow=narrow,admin=admin,passed=True))
    (a.output/"report.json").write_text(json.dumps(dict(checks=results,source_sha256=hashlib.sha256((ROOT/"apps/hagency/bundle/main.splash").read_bytes()).hexdigest(),binary_sha256=hashlib.sha256(a.binary.read_bytes()).hexdigest()),indent=2)+"\n")
    print(json.dumps(results))

if __name__=="__main__":main()
