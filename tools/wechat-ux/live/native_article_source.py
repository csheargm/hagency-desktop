#!/usr/bin/env python3
"""Test source diagnostics/persistence in Rinx with a copied fixture profile.

Requires a previously signed-in @robrix_ux_ test profile; works offline and
never publishes or sends messages. SQLite backup keeps the source profile intact.
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

from native_probe import NativeApp


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/rinx"))
    parser.add_argument("--markdown-file", type=Path, help="Also exercise the macOS file picker with this fixture and an HTML file")
    parser.add_argument("--math-checks", action="store_true", help="Capture all math rows and exercise TeX edit/save/reopen")
    args = parser.parse_args()
    assert (args.profile / "latest_user_id.txt").read_text().strip().startswith("@robrix_ux_")
    root = Path("target/article-source-regressions") / uuid.uuid4().hex
    root.mkdir(parents=True, mode=0o700)
    profile = root / "profile"
    shutil.copytree(args.profile, profile, ignore=shutil.ignore_patterns(
        "*.sqlite3", "*.sqlite3-wal", "*.sqlite3-shm"))
    for path in args.profile.rglob("*.sqlite3"):
        with sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True) as source:
            with sqlite3.connect(profile / path.relative_to(args.profile)) as target:
                source.backup(target)
    (profile / "window_geom_state.json").write_text(json.dumps({
        "inner_size": [430, 820], "position": [50, 50], "is_fullscreen": False}))
    (profile / "ui-language.json").write_text(json.dumps("zh-CN"))
    report = {"passed": False, "checks": [],
              "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest()}
    app = None

    def start(language="zh-CN"):
        (profile / "ui-language.json").write_text(json.dumps(language))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        native = NativeApp(root, port, auto_login=False)
        native.output.mkdir(parents=True)
        native.log = (native.output / "native.log").open("w")
        os.chmod(native.output / "native.log", 0o600)
        env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile.resolve()),
                   MAKEPAD_REMOTE=str(port), MAKEPAD_HIDE_WINDOWS="1", MAKEPAD_NO_FOCUS="1")
        env.pop("MAKEPAD_FOCUS", None)
        if args.markdown_file:
            env.pop("MAKEPAD_HIDE_WINDOWS", None)
        native.process = subprocess.Popen([str(args.binary.resolve())], env=env,
            stdin=subprocess.DEVNULL, stdout=native.log, stderr=subprocess.STDOUT)
        for _ in range(120):
            if native.process.poll() is not None:
                raise RuntimeError("Rinx exited during fixture restoration")
            try:
                assert native.request("/s")["pid"] == native.process.pid
                if any(w["i"] == "discover_tab" for w in native.snap()):
                    return native
            except OSError:
                pass
            time.sleep(.25)
        native.stop()
        raise RuntimeError("Fixture profile did not restore")

    def mark(name):
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def open_library():
        app.click_id("discover_tab")
        app.click_id("discover_article")
        app.click_id("article_continue")
        app.click_id("article_allow")

    def fill(widget, text):
        app.click_id(widget)
        app.request("/k", c="A", cmd=1, wait=1)
        app.request("/t", t=text, wait=1)

    def library():
        return json.loads(next(profile.glob("mini-apps/**/library-v2.json")).read_text())

    def document(document_id):
        return next(d for d in library()["documents"] if d["id"] == document_id)

    def open_draft(title):
        fill("article_search", title)
        row = next(w for w in app.snap() if w.get("t") == title)
        x, y, width, height = row["r"]
        app.click(x + width / 2, y + height / 2)

    def source_value():
        return next(w["val"] for w in app.snap() if w["i"] == "article_markdown")

    def click_immediately(widget):
        row = next(w for w in app.snap() if w["i"] == widget)
        x, y, width, height = row["r"]
        app.request("/click", x=x + width / 2, y=y + height / 2, wait=1)

    def pick_file(path):
        app.click_id("article_import")
        subprocess.run(["osascript", "-", str(app.process.pid), str(path.resolve())], input='''
on run argv
    tell application "System Events"
        tell (first application process whose unix id is (item 1 of argv as integer))
            set frontmost to true
            delay 0.5
            keystroke "g" using {command down, shift down}
            delay 0.5
            keystroke (item 2 of argv)
            delay 0.3
            key code 36
            delay 0.6
            key code 36
        end tell
    end tell
end run
''', text=True, check=True, capture_output=True, timeout=20)
        app.wait_text("文件已导入并保存在本设备", timeout=30)

    try:
        app = start()
        open_library()
        if args.markdown_file:
            path = root / "imported-markdown-test.md"
            path.write_bytes(args.markdown_file.read_bytes())
            pick_file(path)
            imported = next(d for d in library()["documents"] if d["title"] == path.stem)
            assert imported["imported_source"]["text"] == args.markdown_file.read_text()
            assert any(b["kind"] == "markdown" for b in imported["blocks"])
            app.capture("markdown-file-preview")
            if args.math_checks:
                for index in range(2):
                    x,y,width,height=next(w["r"] for w in app.snap() if w["i"] == "article_reader")
                    app.request("/m", k="scroll", x=x+width/2, y=y+height/2, dy=350, wait=1)
                    time.sleep(.25)
                    app.capture(f"math-native-scroll-{index}")
            if args.markdown_file.read_text().rstrip().endswith("### End"):
                for _ in range(30):
                    if any("<h3>End</h3>" in w.get("t", "") for w in app.snap()):
                        break
                    reader = next(w["r"] for w in app.snap() if w["i"] == "article_reader")
                    x, y, width, height = reader
                    app.request("/m", k="scroll", x=x + width / 2, y=y + height / 2, dy=1500, wait=1)
                    time.sleep(.15)
                else:
                    raise AssertionError("Imported Markdown's final section is unreachable")
                app.capture("markdown-file-preview-end")
            app.click_id("css_preview_open")
            for _ in range(100):
                status = next((w.get("t", "") for w in app.snap() if w["i"] == "article_status"), "")
                if status in ("HTML/CSS 排版预览已就绪", "预览已达到长度限制，请返回全文预览阅读完整文章。"):
                    break
                time.sleep(.2)
            else:
                raise AssertionError("Imported Markdown CSS preview failed: " + status)
            app.capture("markdown-css-preview")
            if args.math_checks:
                x,y,width,height=next(w["r"] for w in app.snap() if w["i"] == "css_preview_bitmap")
                app.request("/m", k="scroll", x=x+width/2, y=y+height/2, dy=650, wait=1)
                time.sleep(.3)
                app.capture("math-blitz-bottom")
            app.click_id("article_back")
            app.click_id("article_back")
            app.click_id("article_source")
            assert source_value() == args.markdown_file.read_text()
            if args.math_checks:
                original=args.markdown_file.read_text()
                edited=original.replace("E=mc^2", "E=mc^3")
                assert edited != original
                fill("article_markdown",edited)
                app.click_id("source_apply")
                app.click_id("article_back")
                open_draft(path.stem)
                app.click_id("article_source")
                assert source_value() == edited
                app.click_id("source_apply")
                app.click_id("article_preview")
                app.capture("math-native-edited")
                app.click_id("article_back")
                app.click_id("article_source")
                fill("article_markdown", original)
                mark("math_source_edit_save_reopen_and_preview_refresh")
            app.click_id("source_apply")
            assert not any(w["i"] == "source_apply" for w in app.snap())
            app.click_id("article_back")
            mark("native_file_picker_imports_exact_markdown_and_source_apply_succeeds")

            html_path = root / "imported-html-test.html"
            html = '<!doctype html><html><head><title>Fixture</title></head><body><h1>HTML 文件导入</h1><p>中文 <strong>粗体</strong> 与 <code>inline code</code> 👩‍💻</p><table><tr><th>项目</th><th>数值</th></tr><tr><td>茶</td><td>2</td></tr></table><pre>first line\nsecond line</pre><script>should_not_run()</script></body></html>'
            html_path.write_text(html)
            pick_file(html_path)
            imported_html = next(d for d in library()["documents"] if d["title"] == html_path.stem)
            assert imported_html["blocks"][0]["kind"] == "html"
            assert imported_html["blocks"][0]["text"] == html
            assert any("HTML 文件导入" in w.get("t", "") for w in app.snap())
            app.capture("html-file-preview")
            app.click_id("article_back")
            app.click_id("article_source")
            assert source_value() == html
            html += "\n<p>保存后重新打开</p>"
            fill("article_markdown", html)
            app.click_id("source_apply")
            app.click_id("article_back")
            open_draft(html_path.stem)
            app.click_id("article_source")
            assert source_value() == html
            app.click_id("article_back")
            app.click_id("article_back")
            mark("native_html_file_picker_preview_edit_save_and_reopen")

        app.click_id("article_new")
        title = "源码保留回归 " + root.name[:8]
        fill("article_title", title)
        app.click_id("article_source")
        fill("article_markdown", "## 原始正文\n\n保留 **粗体** 和中文 👩‍💻。")
        app.click_id("source_apply")
        app.click_id("article_save")
        first = next(d for d in library()["documents"] if d["title"] == title)
        source = "# 中文 👩‍💻\n\n<div>原文不能丢</div>\n\n`代码`\n\n- [x] 完成\n"
        app.click_id("article_source")
        fill("article_markdown", source)
        time.sleep(1)
        issues = next(w["t"] for w in app.snap() if w["i"] == "source_issues")
        for text in ("第 1 行", "第 3 行：内嵌 HTML", "第 5 行：行内代码", "第 7 行：任务列表"):
            assert text in issues, issues
        assert library()["source_drafts"][first["id"]] == source
        app.capture("source-diagnostics-zh")
        mark("line_numbers_unicode_and_source_autosave")

        oversized = source + "x" * 24_000
        fill("article_markdown", oversized)
        app.click_id("source_apply")
        assert source_value() == oversized
        assert document(first["id"])["blocks"] == first["blocks"]
        fill("article_markdown", source)
        app.click_id("article_back")
        app.click_id("article_source")
        assert source_value() == source
        mark("oversized_import_preserves_visual_document_and_editable_source")

        source += "\n关闭编辑器前的新内容\n"
        fill("article_markdown", source)
        app.click_id("article_close")
        assert library()["source_drafts"][first["id"]] == source
        app.click_id("discover_article")
        app.click_id("article_continue")
        app.click_id("article_allow")
        open_draft(title)
        app.click_id("article_source")
        assert source_value() == source
        mark("close_and_reauthorize_preserve_unapplied_source")

        app.click_id("article_back")
        app.click_id("article_back")
        app.click_id("article_new")
        second_title = "另一篇源码 " + root.name[:8]
        fill("article_title", second_title)
        app.click_id("article_source")
        second_source = "<p>第二篇 👩‍💻</p>\n"
        fill("article_markdown", second_source)
        app.click_id("article_back")
        app.click_id("article_back")
        second = next(d for d in library()["documents"] if d["title"] == second_title)
        open_draft(title)
        app.click_id("article_source")
        assert source_value() == source
        assert library()["source_drafts"][second["id"]] == second_source
        mark("source_drafts_are_isolated_between_articles")

        source += "\n重启前编辑 👩‍💻\n"
        fill("article_markdown", source)
        app.stop()
        app = None
        assert library()["source_drafts"][first["id"]] == source
        app = start("en")
        open_library()
        open_draft(title)
        app.click_id("article_source")
        assert source_value() == source
        issues = next(w["t"] for w in app.snap() if w["i"] == "source_issues")
        assert "Line 3: Embedded HTML (kept in a source block)" in issues
        app.capture("source-diagnostics-en-after-restart")
        mark("restart_restores_exact_source_and_english_diagnostics")

        fill("article_markdown", "## 应用后的正文\n\n合法 **加粗** 中文 👩‍💻。")
        assert not any(w["i"] == "source_issues" for w in app.snap())
        click_immediately("source_apply")
        # No debounce wait: an image-library reload must not resurrect the source.
        click_immediately("article_images")
        assert first["id"] not in library().get("source_drafts", {})
        app.click_id("article_back")
        saved = document(first["id"])
        assert [b["text"] for b in saved["blocks"]] == ["应用后的正文", "合法 加粗 中文 👩‍💻。"]
        assert first["id"] not in library()["source_drafts"]
        assert library()["source_drafts"][second["id"]] == second_source
        assert all(saved[key] == first[key] for key in ("id", "title", "theme", "cover"))
        mark("successful_import_clears_only_its_source_and_preserves_metadata")
        report["passed"] = True
    finally:
        if app:
            if not report["passed"]:
                try:
                    app.capture("failure")
                except Exception:
                    pass
            app.stop()
        (root / "result.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
        print(json.dumps(dict(report, evidence=str(root)), ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
