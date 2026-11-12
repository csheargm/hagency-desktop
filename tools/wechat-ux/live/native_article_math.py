#!/usr/bin/env python3
"""Hidden native math regression via Makepad's loopback instrumentation.

Uses the real Metal backend with MAKEPAD_HIDE_WINDOWS=1, not the CPU headless
rasterizer. Inputs use /click, /k, /t and /m; pixels use /g, never desktop input
or screenshots. An isolated copy of the signed-in fixture holds all test edits.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import time
import uuid

from PIL import Image, ImageChops
from native_probe import NativeApp


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/rinx"))
    parser.add_argument("--content-checks", action="store_true", help="Also check images, tables, code highlighting, emoji and flowcharts")
    parser.add_argument("--content-case", choices=["images", "linked_image", "table", "code", "emoji", "flow"])
    args = parser.parse_args()
    assert (args.profile / "latest_user_id.txt").read_text().strip().startswith("@robrix_ux_")
    root = Path("target/article-math-headless") / uuid.uuid4().hex
    root.mkdir(parents=True, mode=0o700)
    profile = root / "profile"
    shutil.copytree(args.profile, profile, ignore=shutil.ignore_patterns("*.sqlite3", "*.sqlite3-wal", "*.sqlite3-shm"))
    for path in args.profile.rglob("*.sqlite3"):
        with sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True) as source:
            with sqlite3.connect(profile / path.relative_to(args.profile)) as target:
                source.backup(target)
    (profile / "window_geom_state.json").write_text(json.dumps({"inner_size":[430,820], "position":[50,50], "is_fullscreen":False}))
    (profile / "ui-language.json").write_text(json.dumps("zh-CN"))
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
                if any(w["i"]=="discover_tab" for w in native.snap()):
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
        app.request("/t",t=text,wait=1)

    def library():
        return json.loads(next(profile.glob("mini-apps/**/library-v2.json")).read_text())

    def open_library():
        for name in ["discover_tab","discover_article","article_continue","article_allow"]:app.click_id(name)

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

    def css_ready():
        app.click_id("css_preview_open")
        app.wait_text("HTML/CSS 排版预览已就绪",timeout=60)

    try:
        app=start(); hidden(); mark("zero_visible_windows")
        open_library()
        if args.content_checks or args.content_case:
            logo = "https://pandao.github.io/editor.md/images/logos/editormd-logo-180x180.png"
            badge = "https://img.shields.io/github/stars/pandao/editor.md.svg"
            flow_source = Path("lab/article-editor/render-comparison/evidence/10-diagrams/source.md").read_text().split("```flow\n", 1)[1].split("```", 1)[0]
            cases = {
                "flow": "```flow\n" + flow_source + "```",

                "images": f'![Markdown logo]({logo})\n\n<img src="{badge}" alt="HTML SVG badge">',
                "linked_image": '[![Linked JPEG](https://pandao.github.io/editor.md/examples/images/7.jpg)](https://pandao.github.io/editor.md/images/7.jpg "李健")',
                "table": "| 项目 | 价格 | 数量 |\n| :--- | ---: | :---: |\n| 计算机 | $1600 | 5 |\n| 手机 | $12 | 12 |\n| 管线 | $1 | 234 |",
                "code": '```javascript\nfunction greet(name) {\n  return "你好 " + name; // greeting\n}\n```\n\n```html\n<div class="test">中文</div>\n```\n\n```python\ndef greet(name):\n    return "你好 " + name\n```',
                "emoji": ':smiley: :star: :fa-star: :fa-gear: 😃\n\n:editormd-logo: :editormd-logo-3x: :editormd-logo-5x:\n\nCode stays literal: `:smiley:`',
            }
            def content_pixels(path, widget):
                x,y,w,h=next(w["r"] for w in app.snap() if w["i"]==widget)
                return Image.open(path).convert("RGB").crop((int(x*2),int(y*2),int((x+w)*2),int((y+h)*2)))
            def has_syntax_colors(pixels):
                colors = pixels.getdata()
                blue = sum(b > r+30 and b > g+15 and r < 180 for r,g,b in colors)
                green = sum(g > r+12 and g > b+12 and r < 180 for r,g,b in colors)
                assert blue > 50 and green > 20, ("Missing syntax colors", blue, green)
            for name,source in cases.items():
                if args.content_case and name != args.content_case: continue
                create("Headless " + name,source)
                app.click_id("article_preview")
                if name in ("images", "linked_image", "emoji"):
                    deadline = time.monotonic()+50
                    expected = {"images":2, "linked_image":1, "emoji":3}[name]
                    while time.monotonic()<deadline:
                        if sum(w.get("t", "").count("<rimage") for w in app.snap() if w["ty"]=="Html") >= expected: break
                        time.sleep(.3)
                    assert sum(w.get("t", "").count("<rimage") for w in app.snap() if w["ty"]=="Html") >= expected, "Remote images did not load"
                path=capture("native-"+name)
                if name in ("images", "linked_image"):
                    pixels=content_pixels(path,"article_reader")
                    assert sum(max(r,g,b)-min(r,g,b)>40 for r,g,b in pixels.getdata()) > 2000, "Image pixels missing"
                if name=="linked_image":
                    assert any('<rimage href=' in w.get("t", "") for w in app.snap()), "Linked image target missing"
                if name=="code":
                    has_syntax_colors(content_pixels(path,"article_reader"))
                    assert any("你好" in r["text"] for r in app.ocr()), "Chinese code glyphs are missing"
                if name=="table":
                    pixels=content_pixels(path,"article_reader")
                    assert sum(abs(r-184)<4 and abs(g-194)<4 and abs(b-189)<4 for r,g,b in pixels.getdata()) > 400, "Table borders missing"
                    rows=app.ocr()
                    prices=[r for r in rows if "$1600" in r["text"]]
                    assert prices, "Right-aligned price cell disappeared"
                    assert .38 < prices[0]["box"][0] < .67, prices
                    quantity=next(r for r in rows if r["text"]=="234")
                    assert .72 < quantity["box"][0]+quantity["box"][2]/2 < .88, quantity
                if name=="emoji":
                    assert sum(w.get("t", "").count("<remoji>") for w in app.snap() if w["ty"]=="Html") >= 4
                    pixels=content_pixels(path,"article_reader")
                    assert sum(r>170 and g>100 and b<110 for r,g,b in pixels.getdata()) > 30, "Emoji glyph ink missing"
                if name=="flow":
                    assert any("<rdiagram>" in w.get("t", "") for w in app.snap()), "Flowchart used a code fallback"
                    native_flow=path
                    words=" ".join(r["text"] for r in app.ocr())
                    if "进入后台" not in words:
                        scroll("article_reader",240);capture("native-flow-bottom")
                        words+=" "+" ".join(r["text"] for r in app.ocr())
                        scroll("article_reader",-10000)
                    assert "用户登陆" in words and "进入后台" in words and "yes" in words and "no" in words, words
                css_ready(); path=capture("blitz-"+name)
                if name=="flow":
                    blitz_flow=path
                    words=" ".join(r["text"] for r in app.ocr())
                    if "进入后台" not in words:
                        scroll("css_preview_bitmap",240);capture("blitz-flow-bottom")
                        words+=" "+" ".join(r["text"] for r in app.ocr())
                        scroll("css_preview_bitmap",-10000)
                    assert "用户登陆" in words and "进入后台" in words and "yes" in words and "no" in words, words
                if name=="code": has_syntax_colors(content_pixels(path,"css_preview_bitmap"))
                if name=="emoji":
                    pixels=content_pixels(path,"css_preview_bitmap")
                    assert sum(r>170 and g>100 and b<110 for r,g,b in pixels.getdata()) > 30, "Blitz emoji glyph ink missing"
                app.click_id("article_back");app.click_id("article_back");app.click_id("article_source")
                assert source_value()==source
                if name=="flow":
                    edited=source.replace("用户登陆", "提交订单")
                    fill("article_markdown",edited);app.click_id("source_apply");app.click_id("article_save")
                    app.click_id("article_preview");native_edited=capture("native-flow-edited")
                    assert "提交订单" in " ".join(r["text"] for r in app.ocr())
                    css_ready();blitz_edited=capture("blitz-flow-edited")
                    assert "提交订单" in " ".join(r["text"] for r in app.ocr())
                    for before,after in [(native_flow,native_edited),(blitz_flow,blitz_edited)]:
                        a=Image.open(before).convert("RGB").crop((40,300,800,1500))
                        b=Image.open(after).convert("RGB").crop((40,300,800,1500))
                        assert ImageChops.difference(a,b).getbbox(), "Diagram edit did not change pixels"
                    mark("flow_source_edit_changes_both_preview_images")
                    app.stop();app=None
                    app=start();hidden();open_library();open_draft("Headless flow");app.click_id("article_source")
                    assert source_value()==edited
                    mark("flow_restart_preserves_exact_edited_source")
                    invalid="```flow\na=>start: A\na->missing\n```"
                    fill("article_markdown",invalid);app.click_id("source_apply");app.click_id("article_preview")
                    capture("native-flow-invalid")
                    assert any("Unknown node: missing" in w.get("t", "") for w in app.snap())
                    css_ready();capture("blitz-flow-invalid")
                    words=" ".join(r["text"] for r in app.ocr())
                    assert "Unknown node" in words, words
                    app.click_id("article_back");app.click_id("article_back");app.click_id("article_source")
                    assert source_value()==invalid
                    mark("invalid_flow_keeps_source_and_shows_preview_error")
                app.click_id("article_back");app.click_id("article_back")
                mark(name+"_renders_in_native_and_blitz_and_preserves_source")
        if args.content_case:
            report["passed"]=True
            return
        full=Path("lab/article-editor/render-comparison/source.md").read_text()
        doc=create("Headless full Markdown",full)
        assert len(doc["blocks"])==133
        mark("complete_gist_imports_and_preserves_exact_source")
        if args.content_checks:
            app.click_id("article_preview")
            deadline=time.monotonic()+50
            while time.monotonic()<deadline:
                if any("<rimage" in w.get("t", "") for w in app.snap()):break
                time.sleep(.3)
            capture("full-gist-opening")
            observed=set()
            features={"images":"<rimage", "code":"<rcode", "table":"<table", "emoji":"<remoji", "math":"<rmath", "flow":"<rdiagram"}
            reached_end=False
            for _ in range(40):
                markup=" ".join(w.get("t", "") for w in app.snap() if w["ty"]=="Html")
                for feature,tag in features.items():
                    if tag in markup and feature not in observed:
                        observed.add(feature);capture("full-gist-"+feature)
                if ">End<" in markup:
                    reached_end=True;break
                scroll("article_reader",650)
            assert reached_end and observed==set(features), (reached_end,observed)
            capture("full-gist-end")
            mark("complete_gist_native_preview_reaches_end_with_all_six_rendered_features")
            app.click_id("article_back")
        app.click_id("article_back")
        source=Path("lab/article-editor/render-comparison/evidence/09-math/source.md").read_text()
        title="Headless math"
        doc=create(title,source)
        app.click_id("article_preview")
        native_before=capture("native-math-top")
        scroll("article_reader",350);capture("native-math-middle")
        scroll("article_reader",350);capture("native-math-bottom")
        mark("native_reader_draws_and_scrolls_math_fixture")
        css_ready()
        css_before=capture("blitz-math-top")
        scroll("css_preview_bitmap",650);capture("blitz-math-bottom")
        mark("production_blitz_preview_renders_and_scrolls")
        app.click_id("article_back");app.click_id("article_back");app.click_id("article_source")
        assert source_value()==source
        edited=source.replace("E=mc^2","E=mc^3")
        fill("article_markdown",edited);app.click_id("source_apply")
        app.click_id("article_save")
        app.click_id("article_preview")
        native_after=capture("native-math-edited")
        css_ready();css_after=capture("blitz-math-edited")
        # Only compare the formula content, excluding status, buttons and focus.
        # Widget geometry and title remain fixed between the two captures.
        for before,after,rect in [(native_before,native_after,(40,510,800,780)),
                                  (css_before,css_after,(50,740,810,1100))]:
            a=Image.open(before).convert("RGB").crop(rect)
            b=Image.open(after).convert("RGB").crop(rect)
            assert ImageChops.difference(a,b).getbbox(), "Formula pixels did not change after TeX edit"
        mark("tex_edit_changes_native_and_blitz_formula_pixels")
        assert next(d for d in library()["documents"] if d["id"]==doc["id"])["imported_source"]["text"]==edited
        app.stop();app=None
        app=start();hidden();open_library();open_draft(title);app.click_id("article_source")
        assert source_value()==edited
        mark("restart_preserves_exact_edited_tex")
        report["passed"]=True
    finally:
        if app:
            if not report["passed"]:
                try:capture("failure")
                except Exception:pass
            app.stop()
        (root/"result.json").write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print(json.dumps(dict(report,evidence=str(root)),ensure_ascii=False),flush=True)


if __name__=="__main__":main()
