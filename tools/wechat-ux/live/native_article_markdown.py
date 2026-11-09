#!/usr/bin/env python3
"""Native-only Markdown regression via Makepad's loopback instrumentation.

Uses the real Metal backend with MAKEPAD_HIDE_WINDOWS=1, not the CPU headless
rasterizer. Inputs use /click, /k, /t and /m; pixels use /g, never desktop input
or screenshots. An isolated copy of the signed-in fixture holds all test edits.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import time
import uuid

from PIL import Image, ImageChops
from native_probe import NativeApp


def audit_desktop_runtime_logs(root):
    logs="\n".join(path.read_text(errors="replace") for path in root.glob("native-runs/*/native.log"))
    assert logs and "panicked at" not in logs
    counts={"read_receipts":0,"backwards_pagination":0}
    for line in logs.splitlines():
        if "[E]" not in line:continue
        # Cached chats remain testable with the fixture offline. Accept only
        # refused connections to its loopback server, for these known requests.
        assert "127.0.0.1:18120" in line and "ConnectionRefused" in line, "Unexpected native error; inspect native.log"
        if "src/sliding_sync.rs:" in line and "Failed to send m." in line and "read receipt" in line:
            counts["read_receipts"]+=1
        elif (("src/sliding_sync.rs:" in line and "Error sending backwards pagination request" in line)
              or ("src/home/room_screen.rs:" in line and "Pagination error (backwards)" in line)):
            counts["backwards_pagination"]+=1
        else:
            raise AssertionError("Unexpected native error; inspect native.log")
    return {"expected_offline_network_errors":counts,"unexpected_runtime_errors":0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", type=Path)
    parser.add_argument("--audit-existing",type=Path,help="Recheck logs of a completed desktop interaction run; preserve its original result")
    parser.add_argument("--binary", type=Path, default=Path("target/debug/rinx"))
    parser.add_argument("--skip-small-cases",action="store_true",help="Debug only the navigation and large-document cases")
    parser.add_argument("--diagrams-only",action="store_true",help="Run diagram rendering, editing, errors and wide-window restart checks")
    parser.add_argument("--desktop-entry",action="store_true",help="Force wide layout and exercise the desktop Contacts and article-editor buttons")
    parser.add_argument("--editor-canvas",action="store_true",help="Apply the complete Editor.md sample and inspect the editing canvas itself")
    args = parser.parse_args()
    if args.audit_existing:
        root=args.audit_existing
        original=root/"result.json"
        report=json.loads(original.read_text())
        assert len(report["checks"])==14 and report["checks"][-1]=="desktop_entry_and_draft_survive_process_restart"
        report.update(audit_desktop_runtime_logs(root))
        report["passed"]=True
        report["original_result_sha256"]=hashlib.sha256(original.read_bytes()).hexdigest()
        report["log_audit_note"]="All 14 UI checks completed. Rechecked logs with explicit handling for offline fixture read receipts and backwards pagination; original result.json is preserved."
        output=root/"result-audited.json"
        output.write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print(output)
        return
    if args.profile is None:parser.error("--profile is required for an interaction run")
    assert (args.profile / "latest_user_id.txt").read_text().strip().startswith("@robrix_ux_")
    root = Path("target/article-markdown-headless") / uuid.uuid4().hex
    root.mkdir(parents=True, mode=0o700)
    profile = root / "profile"
    shutil.copytree(args.profile, profile, ignore=shutil.ignore_patterns("*.sqlite3", "*.sqlite3-wal", "*.sqlite3-shm"))
    for path in args.profile.rglob("*.sqlite3"):
        with sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True) as source:
            with sqlite3.connect(profile / path.relative_to(args.profile)) as target:
                source.backup(target)
    (profile / "window_geom_state.json").write_text(json.dumps({"inner_size":[430,820], "position":[50,50], "is_fullscreen":False}))
    (profile / "ui-language.json").write_text(json.dumps("zh-CN"))
    if args.desktop_entry or args.editor_canvas:
        (profile / "window_geom_state.json").write_text(json.dumps({"inner_size":[1154,800], "position":[50,50], "is_fullscreen":False}))
        for path in profile.glob("*/persistent_state/latest_app_state.json"):
            state=json.loads(path.read_text())
            state.setdefault("app_prefs",{})["view_mode"]="ForceWide"
            path.write_text(json.dumps(state))
    swift = root / "visible-windows.swift"
    swift.write_text('''import CoreGraphics
import Foundation
let pid = Int32(CommandLine.arguments[1])!
let windows = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] ?? []
print(windows.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }.count)
''')
    helper = root / "visible-windows"
    subprocess.run(["swiftc", str(swift), "-o", str(helper)], check=True, capture_output=True)
    report = {"passed":False, "mode":"Makepad instrumentation / hidden Metal backend",
              "binary_sha256":hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "checks":[], "visible_window_samples":[], "captures":[]}
    app = None

    def mark(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def hidden():
        count = int(subprocess.check_output([str(helper), str(app.process.pid)]))
        report["visible_window_samples"].append(count)
        assert count == 0, "Instrumented Rinx exposed a desktop window"

    def capture(name):
        hidden()
        path = app.capture(name)
        snap = app.snap()
        (root / (name + ".json")).write_text(json.dumps(snap, ensure_ascii=False, indent=2))
        report["captures"].append(name)
        return path

    def start():
        with socket.socket() as s:
            s.bind(("127.0.0.1",0)); port=s.getsockname()[1]
        native=NativeApp(root,port,auto_login=False)
        native.output.mkdir(parents=True)
        native.log=(native.output/"native.log").open("w")
        os.chmod(native.output/"native.log",0o600)
        env=dict(os.environ,HAGENCY_DESKTOP_DATA_DIR=str(profile.resolve()),
                 MAKEPAD_REMOTE=str(port),MAKEPAD_HIDE_WINDOWS="1",MAKEPAD_NO_FOCUS="1")
        env.pop("MAKEPAD_FOCUS",None)
        native.process=subprocess.Popen([str(args.binary.resolve())],env=env,stdin=subprocess.DEVNULL,stdout=native.log,stderr=subprocess.STDOUT)
        for _ in range(120):
            assert native.process.poll() is None, "Instrumented Rinx exited"
            try:
                assert native.request("/s")["pid"]==native.process.pid
                if any(w["i"]==("article_editor_button" if args.desktop_entry or args.editor_canvas else "discover_tab") for w in native.snap()):
                    if "[E] crates/article-makepad" in (native.output/"native.log").read_text(errors="replace"):
                        native.stop()
                        raise AssertionError("Article widget script error")
                    return native
            except OSError:pass
            time.sleep(.25)
        native.stop()
        raise RuntimeError("Fixture did not restore")

    def fill(widget,text):
        app.click_id(widget)
        app.request("/k",c="A",cmd=1,wait=1)
        for offset in range(0,len(text),1000):
            app.request("/t",t=text[offset:offset+1000],wait=1)

    def library():
        return json.loads(next(profile.glob("mini-apps/**/library-v2.json")).read_text())

    def open_library():
        entry=["article_editor_button"] if args.desktop_entry or args.editor_canvas else ["discover_tab","discover_article"]
        for name in entry+["article_continue","article_allow"]:app.click_id(name)

    def create(title,source):
        app.click_id("article_new")
        fill("article_title",title)
        app.click_id("article_source")
        fill("article_markdown",source)
        app.click_id("source_apply")
        assert not any(w["i"]=="source_apply" for w in app.snap())
        app.click_id("article_save")
        doc=next(d for d in library()["documents"] if d["title"]==title)
        assert doc["imported_source"]["text"]==source
        return doc

    def open_draft(title):
        fill("article_search",title)
        x,y,w,h=next(w["r"] for w in app.snap() if w.get("t")==title)
        app.click(x+w/2,y+h/2)

    def source_value():
        return next(w["val"] for w in app.snap() if w["i"]=="article_markdown")

    def scroll(widget,delta):
        x,y,w,h=next(w["r"] for w in app.snap() if w["i"]==widget)
        app.request("/m",k="scroll",x=x+w/2,y=y+h/2,dy=delta,wait=1)
        time.sleep(.3)

    def pixels(path):
        x,y,w,h=next(w["r"] for w in app.snap() if w["i"]=="article_reader")
        return Image.open(path).convert("RGB").crop((int(x*2),int(y*2),int((x+w)*2),int((y+h)*2)))

    def native_markup():
        return "\n".join(w.get("t", "") for w in app.snap() if w["ty"]=="Html")

    def words():
        return " ".join(r["text"] for r in app.ocr())

    def back_to_library():
        app.click_id("article_back")
        app.click_id("article_back")

    def wide_diagram_restart(source):
        nonlocal app
        app.stop();app=None
        (profile / "window_geom_state.json").write_text(json.dumps({"inner_size":[920,820], "position":[50,50], "is_fullscreen":False}))
        app=start();hidden();open_library();open_draft("Native sequence")
        app.click_id("article_source")
        assert source_value()==source.replace("Says Hello","你好世界")
        app.click_id("article_back");app.click_id("article_preview");capture("sequence-wide-restarted")
        assert "你好世界" in words() and "Andrew" in words(),words()
        mark("diagram_source_and_pixels_survive_restart_at_wide_size")

    try:
        app=start(); hidden(); mark("zero_visible_windows")
        if args.desktop_entry:
            def wait_chat(title):
                deadline=time.monotonic()+20
                while time.monotonic()<deadline:
                    if any(w["i"]=="desktop_chat_title" and w.get("t")==title for w in app.snap()):return
                    time.sleep(.25)
                raise AssertionError("Desktop conversation did not open: "+title)

            assert not any(w["i"]=="discover_tab" for w in app.snap()), "Test must use actual desktop navigation"
            x,y,w,h=next(w["r"] for w in app.snap() if w["i"]=="contacts_button")
            assert x<100 and y<400 and w>20 and h>20
            app.request("/m",k="move",x=x+w/2,y=y+h/2,wait=1)
            app.wait_text("通讯录")
            app.click_id("contacts_button")
            app.wait_text("新的朋友")
            assert any(w["i"]=="contacts_page" and w["r"][2]>900 for w in app.snap())
            mark("desktop_contacts_button_opens_full_width_page")
            fixture=json.loads((args.profile.parent/"fixture.json").read_text())
            emma=fixture["users"]["emma"]["user_id"]
            deadline=time.monotonic()+90
            while time.monotonic()<deadline:
                names=[w for w in app.snap() if w["i"]=="name" and w.get("t") in (emma,"Emma Wilson")]
                if names:break
                time.sleep(.25)
            assert names, "Cached Matrix direct contacts must remain usable offline"
            capture("desktop-contacts")
            fill("search",emma)
            names=[w for w in app.snap() if w["i"]=="name"]
            assert len(names)==1 and names[0]["t"] in (emma,"Emma Wilson")
            x,y,w,h=names[0]["r"];app.click(x+w/2,y+h/2)
            app.wait_text(emma)
            assert any(w["i"]=="message" for w in app.snap())
            capture("desktop-contact-profile")
            mark("desktop_contacts_filter_and_profile")
            app.click_text("消息")
            wait_chat("Emma Wilson")
            capture("desktop-contact-chat")
            assert "详细资料" not in words()
            mark("desktop_contact_opens_existing_chat")
            app.click_id("contacts_button")
            app.click_id("left");fill("search","")
            app.click_id("new_friends")
            assert not any(w["i"]=="new_friends" for w in app.snap())
            assert any(w["i"]=="search" for w in app.snap())
            capture("desktop-find-contact")
            mark("desktop_contact_directory_search_page")
            app.click_id("left");app.click_id("groups")
            app.wait_text("Design Studio")
            assert any(w["i"]=="group_row" for w in app.snap())
            capture("desktop-contact-groups")
            mark("desktop_contacts_joined_groups")
            app.click_text("Design Studio")
            wait_chat("Design Studio")
            capture("desktop-group-chat")
            assert not any(r["text"]=="群聊" for r in app.ocr())
            mark("desktop_contact_group_opens_chat")
            app.click_id("contacts_button")
            app.wait_text("Design Studio")
            app.click_id("left")
            app.click_id("home_button")
            mark("desktop_contacts_home_round_trip")
            for _ in range(3):
                app.click_id("contacts_button");fill("search",emma)
                row=next(w for w in app.snap() if w["i"]=="name" and w.get("t") in (emma,"Emma Wilson"))
                x,y,w,h=row["r"];app.click(x+w/2,y+h/2)
                app.click_text("消息");wait_chat("Emma Wilson")
                app.click_id("contacts_button");app.click_id("left");fill("search","")
                app.click_id("groups");app.wait_text("Design Studio")
                app.click_text("Design Studio");wait_chat("Design Studio")
                app.click_id("contacts_button");app.click_id("left");app.click_id("home_button")
            capture("desktop-contacts-repeated-switches")
            mark("desktop_contacts_repeated_chat_switches")
            x,y,w,h=next(w["r"] for w in app.snap() if w["i"]=="article_editor_button")
            assert x<100 and y<400 and w>20 and h>20
            app.request("/m",k="move",x=x+w/2,y=y+h/2,wait=1)
            app.wait_text("文章编辑器")
            capture("desktop-article-button")
            mark("desktop_sidebar_editor_button_and_translated_tooltip")
            open_library()
            capture("desktop-article-library")
            mark("desktop_button_opens_article_library")
            source="# Desktop editor\n\n$E=mc^2$\n\n```mermaid\nflowchart LR\nA[编辑] --> B[预览]\n```\n"
            create("Desktop article entry",source)
            app.click_id("article_preview")
            capture("desktop-article-preview")
            assert "<rdiagram>" in native_markup() and "<rmath>" in native_markup()
            assert "编辑" in words() and "预览" in words()
            mark("desktop_create_save_and_native_preview")
            app.click_id("article_close")
            assert any(w["i"]=="article_editor_button" for w in app.snap())
            open_library();open_draft("Desktop article entry")
            app.click_id("article_source");assert source_value()==source
            mark("desktop_close_reopen_preserves_draft_source")
            app.stop();app=None
            app=start();hidden();open_library();open_draft("Desktop article entry")
            app.click_id("article_source");assert source_value()==source
            capture("desktop-article-source-restarted")
            mark("desktop_entry_and_draft_survive_process_restart")
            report.update(audit_desktop_runtime_logs(root))
            report["passed"]=True
            return
        open_library()
        if args.editor_canvas:
            source=Path("lab/article-editor/render-comparison/source.md").read_text()
            create("Editor canvas regression",source)
            capture("editor-applied-top")
            assert re.search(r"<h1[^>]*>Editor.md</h1>",native_markup()), "Source → Apply still shows raw Markdown in the editing canvas"
            mark("source_apply_renders_in_editor_canvas")

            def canvas_widgets():
                widgets=app.snap()
                x,y,w,h=next(row["r"] for row in widgets if row["i"]=="article_blocks")
                return [row for row in widgets if row["r"][0]>=x and row["r"][0]<x+w
                        and min(row["r"][1]+row["r"][3],y+h)-max(row["r"][1],y)>=min(64,row["r"][3])]

            required={"image":("ArticleImage","MarkdownImage"), "code":("ArticleCode","MarkdownCode"),
                      "table":("ArticleCell","MarkdownCell"), "emoji":("ArticleEmoji","MarkdownEmoji"),
                      "math":("ArticleMath","MarkdownMath"), "diagram":("ArticleDiagram","MarkdownDiagram")}
            seen=set();diagrams=set();inline_math=False;code_languages=set()
            for step in range(190):
                visible=canvas_widgets()
                for name,kinds in required.items():
                    if name not in seen and any(w["ty"] in kinds and (name!="image" or w["r"][3]>=64) for w in visible):
                        capture("editor-"+name)
                        seen.add(name)
                markup="\n".join(w.get("t","") for w in visible if w["ty"]=="Html" and w["i"]=="body")
                code_rows=[w for w in visible if w["ty"] in required["code"] and w["r"][3]>=100]
                for language in ("javascript","html"):
                    if language not in code_languages and code_rows and re.search(r"<rcode>[^:]+:"+language.encode().hex()+":",markup):
                        path=capture("editor-code-"+language)
                        x,y,w,h=code_rows[0]["r"]
                        crop=Image.open(path).convert("RGB").crop((int(x*2),int(y*2),int((x+w)*2),int((y+h)*2)))
                        assert sum(max(rgb)-min(rgb)>50 for rgb in crop.getdata())>30, "Syntax highlighting must produce colored code pixels"
                        code_languages.add(language)
                if "<rdiagram>" in markup:
                    diagrams.update(re.findall(r"<rdiagram>(.*?)</rdiagram>",markup))
                if "行内的公式" in markup:
                    assert markup.count("<rmath>0:") == 2, "Double-dollar math inside a sentence must remain inline"
                    inline_math=True
                if any(w.get("t","").strip()=="End" for w in visible):break
                scroll("article_blocks",280)
            else:raise AssertionError("Could not scroll to the end of the complete sample")
            assert seen==set(required), (seen,required)
            assert len(diagrams)==2, "Both flow and sequence diagrams must render on the canvas"
            assert inline_math
            assert code_languages=={"javascript","html"}
            capture("editor-end")
            for name in required:mark(name+"_rendered_on_editing_canvas")
            mark("flow_and_sequence_rendered_on_editing_canvas")
            mark("double_dollar_math_inside_sentences_stays_inline")
            for language in sorted(code_languages):mark(language+"_highlighting_on_editing_canvas")
            app.click_id("article_source");assert source_value()==source
            mark("full_sample_source_unchanged_after_rendering")
            app.click_id("article_back");app.click_id("article_preview")
            capture("editor-full-preview")
            assert re.search(r"<h1[^>]*>Editor.md</h1>",native_markup())
            mark("full_preview_still_renders")
            back_to_library()

            editable="# Editable heading\n\n`code` [ref][target]\n\n[target]: https://example.org\n"
            create("Canvas block editing",editable)
            # A source block has its own select-all; changing it must not erase
            # the rest of the article or its reference definitions.
            toggle=min((w for w in canvas_widgets() if w["i"]=="source_toggle"),key=lambda w:w["r"][1])
            x,y,w,h=toggle["r"];app.click(x+w/2,y+h/2)
            rich=next(w for w in canvas_widgets() if w["i"]=="rich" and w.get("t","").startswith("# Editable heading"))
            x,y,w,h=rich["r"];app.click(x+w/2,y+h/2)
            app.request("/k",c="A",cmd=1,wait=1)
            app.request("/t",t="# Edited heading",wait=1)
            toggle=next(w for w in canvas_widgets() if w["i"]=="source_toggle" and w.get("t")=="显示渲染")
            x,y,w,h=toggle["r"];app.click(x+w/2,y+h/2)
            app.click_id("article_save")
            capture("editor-block-edited")
            assert re.search(r"<h1[^>]*>Edited heading</h1>",native_markup())
            assert 'href="https://example.org"' in native_markup()
            doc=next(d for d in library()["documents"] if d["title"]=="Canvas block editing")
            assert len(doc["blocks"])==2 and "https://example.org" in doc["reference_definitions"]
            app.click_id("article_source");edited=source_value();assert "# Edited heading" in edited
            mark("edit_one_block_and_render_preserves_other_blocks_and_references")
            app.stop();app=None
            app=start();hidden();open_library();open_draft("Canvas block editing")
            capture("editor-block-restarted")
            assert re.search(r"<h1[^>]*>Edited heading</h1>",native_markup())
            app.click_id("article_source");assert source_value()==edited
            mark("edited_block_source_and_render_survive_restart")
            report.update(audit_desktop_runtime_logs(root))
            report["passed"]=True
            return
        cases={
            "formatting": "# Native headings\n\n[**Bold** and *italic* link](#target)\n\nThe <abbr title=\"Hyper Text\">HTML</abbr> specification.\n\n~~deleted~~ and **strong** and *italic*.\n\nX<sub>2</sub> Y<sup>2</sup> &copy; &euro;\n\nsoft\nbreak\n\nhard  \nbreak\n\n### target",
            "table": "| 项目 | 价格 | 数量 |\n| :--- | ---: | :---: |\n| 计算机 | $1600 | 5 |\n| 手机 | $12 | 12 |\n| 管线 | $1 | 234 |",
            "code": '```javascript\nfunction greet(name) {\n  return "你好 " + name; // greeting\n}\n```\n\n```html\n<div class="test">中文</div>\n```\n\n```python\ndef greet(name):\n    return "你好 " + name\n```',
            "emoji": ':smiley: :star: :fa-star: :fa-gear: 😃\n\nCode stays literal: `:smiley:`',
            "math": r"$E=mc^2$ inline."+"\n\n"+r"$$\frac{a+b}{c}=\sqrt{2}$$"+"\n\n```math\n"+r"\sum_{k=1}^{n} k^2"+"\n```",
            "lists": "0. zero\n1. first\n   - nested **bold**\n   - child\n2. second\n\n- [x] Finished\n- [ ] Pending\n\n> Outer quote\n>\n> > Inner quote",
            "alerts": "> [!WARNING]\n> Keep this **visible**.\n\nTerm\n: Description remains visible.\n\n<div><span>Raw container text</span></div>",
        }
        flow=Path("lab/article-editor/render-comparison/evidence/10-diagrams/source.md").read_text().split("```flow\n",1)[1].split("```",1)[0]
        cases["flow"]="```flow\n"+flow+"```"
        cases["sequence"]="```seq\nAndrew->China: Says Hello\nNote right of China: China thinks\\nabout it\nChina-->Andrew: How are you?\nAndrew->>China: I am good thanks!\n```"
        cases["sequence-alias"]='```seq\nTitle: 中文时序\nparticipant "客户端 #1 as C" # comment\nparticipant "服务端 as S"\nC->S: 发送消息\nNote over C,S: 处理中\nS-->>C: 完成\n```'
        cases["mermaid"]="```mermaid\nflowchart TD\nsubgraph 登录\nA[用户] --> B{通过?}\nB -->|Yes| C[完成]\nB -->|No| A\nend\n```"
        cases["mermaid-sequence"]="```mermaid\nsequenceDiagram\nparticipant A as 客户端\nparticipant B as 服务端\nloop Retry\nA->>+B: Request\nalt Success\nB-->>-A: Complete\nelse Failure\nB-)A: Retry later\nend\nend\n```"
        cases["mermaid-invalid"]="```mermaid\nsequenceDiagram\nloop unclosed\nA->>B: hello\n```"
        if args.diagrams_only:
            cases={k:v for k,v in cases.items() if k in ("flow","sequence","sequence-alias","mermaid","mermaid-sequence","mermaid-invalid")}
        for name,source in ([] if args.skip_small_cases else cases.items()):
            create("Native "+name,source);app.click_id("article_preview")
            assert not any(w["i"]=="css_preview_open" for w in app.snap()), "Markdown still offers Blitz"
            path=capture(name)
            text=words();markup=native_markup()
            if name=="formatting":
                assert "Bold" in text and "italic" in text and "specification" in text, text
                assert "<abbr" not in markup
                assert '<strong><a href="#target">Bold</a></strong>' in markup, markup
            elif name=="table":
                assert "$1600" in text and "234" in text, text
                assert sum(abs(r-184)<4 and abs(g-194)<4 and abs(b-189)<4 for r,g,b in pixels(path).getdata())>400
            elif name=="code":
                assert "你好" in text and "e4b8ade69687" in markup, text
                values=list(pixels(path).getdata())
                assert sum(b>r+30 and b>g+15 and r<180 for r,g,b in values)>50
                assert sum(g>r+12 and g>b+12 and r<180 for r,g,b in values)>20
            elif name=="emoji":
                assert markup.count("<remoji>")>=4
                assert sum(r>170 and g>100 and b<110 for r,g,b in pixels(path).getdata())>30
            elif name=="math":
                assert markup.count("<rmath>")>=3, markup
                assert "\\frac" not in text, text
            elif name=="lists":
                assert "zero" in text and "nested" in text and "Pending" in text, text
                assert '<ol start="0">' in markup and "☑" in markup and "☐" in markup,markup
            elif name=="alerts":
                assert "Warning" in text and "visible" in text and "Raw container text" in text,text
            elif name=="flow":
                assert "<rdiagram>" in markup
                if "进入后台" not in text:
                    scroll("article_reader",240);capture("flow-bottom");text+=" "+words()
                assert "用户登陆" in text and "进入后台" in text and "yes" in text and "no" in text,text
            elif name in ("sequence","sequence-alias","mermaid","mermaid-sequence"):
                assert "<rdiagram>" in markup and "<rcode>" not in markup,markup
                assert "Diagram:" not in text,text
                if name=="sequence":
                    assert "Andrew" in text and "China" in text and "Says Hello" in text and "about it" in text,text
                elif name=="sequence-alias":
                    assert "中文时序" in text and "发送消息" in text and "处理中" in text and "完成" in text,text
                elif name=="mermaid":
                    assert "用户" in text and "完成" in text and "Yes" in text and "No" in text,text
                else:
                    assert "客户端" in text and "服务端" in text and "Request" in text and "Complete" in text,text
                assert sum(max(r,g,b)-min(r,g,b)>15 for r,g,b in pixels(path).getdata())>1000
            elif name=="mermaid-invalid":
                assert "Diagram:" in text and "unclosed" in text,text
                assert "<rdiagram>" not in markup,markup
            if name in ("code","emoji","math","table"):
                app.request("/k",c="A",cmd=1,wait=1)
                app.request("/g")  # second frame applies the new selection to custom widgets
                capture(name+"-selected")
            mark(name+"_native_pixels")
            if name=="sequence":
                before=pixels(path)
                edited=source.replace("Says Hello","你好世界")
                app.click_id("article_back");app.click_id("article_source")
                assert source_value()==source
                fill("article_markdown",edited);app.click_id("source_apply");app.click_id("article_save")
                app.click_id("article_preview")
                after=capture("sequence-edited")
                assert "你好世界" in words() and "Says Hello" not in words(),words()
                assert ImageChops.difference(before,pixels(after)).getbbox()
                mark("sequence_edit_regenerates_diagram_pixels")
                source=edited
            if name in ("sequence","sequence-alias","mermaid","mermaid-sequence","mermaid-invalid"):
                app.click_id("article_back");app.click_id("article_source")
                assert source_value()==source
                app.click_id("article_back");app.click_id("article_back")
                mark(name+"_source_unchanged")
                continue
            back_to_library()
        if args.diagrams_only:
            wide_diagram_restart(cases["sequence"])
            assert "panicked at" not in (app.output/"native.log").read_text(errors="replace")
            report["passed"]=True
            return
        source="# Navigation\n\n[TOC]\n\nFootnote[^n].\n\n"+"\n\n".join("Paragraph "+str(i)+" with ordinary content." for i in range(25))+"\n\n## Destination\n\nDESTINATION BODY\n\n[^n]: FOOTNOTE DESTINATION.\n"
        create("Native navigation",source);app.click_id("article_preview");capture("navigation-top")
        assert "Table of contents" in words()
        # Use the actual inline link rectangle, not a scroll command.
        app.click_text("Destination")
        capture("navigation-heading")
        assert "DESTINATION BODY" in words(), words()
        mark("toc_link_scrolls_to_heading")
        scroll("article_reader",-10000)
        target=next(w for w in app.ocr() if "Footnote" in w["text"])
        x,y,w,h=target["box"];app.click((x+w*.92)*430,(y+h*.25)*820)
        capture("navigation-footnote")
        assert "FOOTNOTE DESTINATION" in words(), words()
        mark("footnote_link_scrolls_to_definition")
        back_to_library()
        # Whole fixture, byte-for-byte, and its last block beyond one GPU texture.
        source=Path("lab/article-editor/render-comparison/source.md").read_text()
        create("Native full fixture",source);app.click_id("article_preview")
        deadline=time.monotonic()+70
        while "<rimage>" not in native_markup() and time.monotonic()<deadline:time.sleep(.3)
        path=capture("full-fixture-top")
        assert "<rimage>" in native_markup(), "User fixture image did not load"
        assert sum(max(r,g,b)-min(r,g,b)>40 for r,g,b in pixels(path).getdata())>2000
        mark("remote_image_draws_native_pixels")
        for _ in range(4):scroll("article_reader",10000)
        capture("full-fixture-end")
        assert "End" in words(), words()
        mark("full_user_fixture_reaches_end")
        app.click_id("article_back");app.click_id("article_source")
        assert source_value()==source
        mark("full_user_fixture_source_unchanged")
        app.click_id("article_back");app.click_id("article_back")
        # The previous 24 KB limit rejected otherwise valid Markdown documents.
        source="\n\n".join(f"## Chapter {i}\n\n"+"Ordinary Markdown text 中文. "*30 for i in range(80))+"\n\nLONG DOCUMENT END"
        assert len(source.encode())>24000
        create("Native long document",source);app.click_id("article_preview")
        for _ in range(12):scroll("article_reader",10000)
        capture("long-document-end")
        assert "LONG DOCUMENT END" in words(), words()
        mark("long_document_virtualized_without_texture_clipping")
        app.stop();app=None
        app=start();hidden();open_library();open_draft("Native full fixture")
        app.click_id("article_source");assert source_value()==Path("lab/article-editor/render-comparison/source.md").read_text()
        mark("source_survives_process_restart")
        if not args.skip_small_cases:
            wide_diagram_restart(cases["sequence"])
        log=(app.output/"native.log").read_text(errors="replace")
        assert "[E] crates/article-makepad" not in log and "panicked at" not in log,log[-2000:]
        report["passed"]=True
    finally:
        if app and not report["passed"] and app.process.poll() is None:
            try:capture("failure")
            except Exception:pass
        if app:app.stop()
        (root/"result.json").write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print(root/"result.json",flush=True)

if __name__=="__main__":main()
